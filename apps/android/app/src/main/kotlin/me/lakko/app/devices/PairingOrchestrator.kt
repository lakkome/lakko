// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.Context
import me.lakko.app.auth.DeviceIdentityStore
import uniffi.lakko_ffi_uniffi.DeviceGroup
import uniffi.lakko_ffi_uniffi.Identity

/**
 * High-level orchestration for the two add-device sides on
 * Android (Phase 2a3b4).
 *
 * Mirror of:
 *
 *   * `apps/web/src/devices/orchestrator.ts`
 *   * `apps/ios/Lakko/Devices/PairingOrchestrator.swift`
 *
 * Wraps the per-call sequence (DeviceIdentity build, pairing
 * codec, DeviceGroup operations, DS-mailbox round-trip,
 * snapshot persistence) into two flat call shapes the Compose
 * view invokes:
 *
 *   * [prepareJoinFlow] — joining-device side. Returns a
 *     long-lived [JoinSession] handle; the view renders the QR
 *     from `qrPayload` and drives `awaitWelcome` →
 *     `accept(welcomeBytes)`.
 *
 *   * [runScanFlow] — scanner-device side. One-shot suspend
 *     function: decode the scanned QR, add the new device's
 *     leaf, publish the Welcome, persist the snapshot.
 */
public object PairingOrchestrator {

    /**
     * Joining-side session handle. The Compose view:
     *
     *   1. Reads `qrPayload`, feeds it into the ZXing QR
     *      generator to render the QR bitmap.
     *   2. Calls [awaitWelcome] inside a coroutine to long-poll
     *      the rendezvous slot; on
     *      [WelcomeCourier.WelcomeOutcome.Received] calls
     *      [accept] and surfaces success.
     *   3. On cancel / timeout / error calls [dispose] so the
     *      DeviceIdentity + post-join DeviceGroup FFI handles
     *      are freed.
     *
     * Idempotent on `dispose()` — safe to call multiple times.
     */
    public class JoinSession internal constructor(
        /** Wire-format pairing QR bytes for the rendering
         *  layer. */
        public val qrPayload: ByteArray,
        /** 32-byte rendezvous pseudonym surfaced for debug. */
        public val rendezvous: ByteArray,
        private val identity: Identity,
        private val accountIdBytes: ByteArray,
        private val accountIdB64: String,
        private val context: Context,
    ) {
        private var group: DeviceGroup? = null
        private var disposed = false

        /** Long-poll the rendezvous mailbox. */
        public suspend fun awaitWelcome(
            timeoutMillis: Long = WelcomeCourier.DEFAULT_TIMEOUT_MS,
        ): WelcomeCourier.WelcomeOutcome = WelcomeCourier.pollForWelcome(
            rendezvousPseudonym = rendezvous,
            timeoutMillis = timeoutMillis,
        )

        /**
         * Drive the terminal joining-side step: join the
         * DeviceGroup with the Welcome the scanner published,
         * persist the resulting snapshot.
         *
         * Throws when the embedded `group_id` doesn't match the
         * deterministic `BLAKE3("lakko-device-group/v1" ||
         * account_id)` value for this account — defensive
         * against a misrouted Welcome from a different user.
         */
        public fun accept(welcomeBytes: ByteArray) {
            check(!disposed) { "JoinSession.accept called after dispose" }
            // The deterministic group_id check inside
            // DeviceGroup.joinFromWelcome rejects a misrouted
            // Welcome before any state advance.
            group = DeviceGroup.joinFromWelcome(
                welcome = welcomeBytes,
                identity = identity,
                accountId = accountIdBytes,
            )
            DeviceGroupLifecycle.persistSnapshot(
                context = context,
                identity = identity,
                accountIdB64 = accountIdB64,
            )
        }

        /** Release the FFI handles backing this session. Safe
         *  to call multiple times. */
        public fun dispose() {
            if (disposed) return
            disposed = true
            try {
                group?.close()
            } catch (_: Throwable) {
                // best-effort
            }
            group = null
            try {
                identity.close()
            } catch (_: Throwable) {
                // best-effort
            }
        }
    }

    /**
     * Joining-device side entry point. Builds the
     * DeviceIdentity from the cached seed, generates a fresh
     * rendezvous slot, produces a KeyPackage, encodes the
     * pairing QR. The view drives the rest via the returned
     * [JoinSession].
     *
     * The caller owns [JoinSession.dispose] — the typical
     * call site sits inside a Compose `DisposableEffect`
     * whose `onDispose` block invokes it.
     */
    public fun prepareJoinFlow(
        context: Context,
        accountIdBytes: ByteArray,
        accountIdB64: String,
    ): JoinSession {
        val seed = DeviceIdentityStore.ensureDeviceIdentitySeed(
            context = context,
            accountIdB64 = accountIdB64,
        )
        val identity = Identity.fromSeed(seed)
        // Best-effort scrub of the raw seed bytes; the
        // Identity has them folded into its in-memory state
        // and no longer needs the heap buffer.
        seed.fill(0)
        val keyPackage = identity.createKeyPackage()
        val rendezvous = PairingCodec.generateRendezvousPseudonym()
        val qrPayload = PairingCodec.buildPairingQrPayload(
            accountId = accountIdBytes,
            keyPackage = keyPackage,
            rendezvousPseudonym = rendezvous,
        )
        return JoinSession(
            qrPayload = qrPayload,
            rendezvous = rendezvous,
            identity = identity,
            accountIdBytes = accountIdBytes,
            accountIdB64 = accountIdB64,
            context = context.applicationContext,
        )
    }

    /** Result of [runScanFlow]. */
    public data class ScanResult(
        /** Number of devices in the local DeviceGroup AFTER
         *  the add. Best-effort estimate today — full
         *  `devices()` accessor lands alongside 2a3c. */
        val memberCount: Int,
    )

    /**
     * Scanner-device side entry point. One-shot: decode the
     * QR, run `addDeviceFromWire`, publish the Welcome,
     * persist the snapshot.
     *
     * Throws the inner error verbatim when:
     *   * The QR was scanned against a different account.
     *   * The QR carries a newer-version payload than this
     *     binary recognises.
     *   * The Welcome PUT fails (DS-mailbox unreachable).
     *   * The DeviceGroup state on this device is corrupt.
     */
    public suspend fun runScanFlow(
        context: Context,
        scannedBytes: ByteArray,
        accountIdBytes: ByteArray,
        accountIdB64: String,
    ): ScanResult {
        val decoded = PairingCodec.parsePairingQrPayload(
            wireBytes = scannedBytes,
            expectedAccountId = accountIdBytes,
        )
        val seed = DeviceIdentityStore.ensureDeviceIdentitySeed(
            context = context,
            accountIdB64 = accountIdB64,
        )
        val ensured = DeviceGroupLifecycle.ensureDeviceGroup(
            context = context,
            accountIdBytes = accountIdBytes,
            accountIdB64 = accountIdB64,
            deviceIdentitySeed = seed,
        )
        // Scrub the raw seed; the Identity owns the secret
        // material from here on.
        seed.fill(0)

        try {
            val addition = ensured.group.addDeviceFromWire(
                identity = ensured.identity,
                keyPackage = decoded.keyPackage,
            )
            WelcomeCourier.publishWelcome(
                rendezvousPseudonym = decoded.rendezvousPseudonym,
                welcomeBytes = addition.welcome,
            )
            // TODO(2a3b4-followup): once the DS group journal
            // is wired for DeviceGroup commits, broadcast
            // `addition.commit` here too so any other
            // already-paired devices on this account advance
            // to the new epoch. For the founder + first-pair
            // case the commit is a no-op (the founder is the
            // only existing member, and the merge-pending-
            // commit already advanced its local view inside
            // addDeviceFromWire).
            DeviceGroupLifecycle.persistSnapshot(
                context = context,
                identity = ensured.identity,
                accountIdB64 = accountIdB64,
            )
            return ScanResult(memberCount = if (ensured.bootstrapped) 2 else 0)
        } finally {
            try {
                ensured.group.close()
            } catch (_: Throwable) {
                // best-effort
            }
            try {
                ensured.identity.close()
            } catch (_: Throwable) {
                // best-effort
            }
        }
    }
}
