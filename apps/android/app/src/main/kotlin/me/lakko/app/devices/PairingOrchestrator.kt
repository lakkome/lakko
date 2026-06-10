// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.Context
import me.lakko.app.auth.AuthClient
import me.lakko.app.auth.DeviceIdentityStore
import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.ds.DsClient
import uniffi.lakko_ffi_uniffi.DeviceGroup
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.openWithKey
import uniffi.lakko_ffi_uniffi.sealWithKey

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
     * ADR-0018 factor (a) — what the joiner recovered from the
     * scanner's sealed rendezvous row. `phraseSeed` is the 64-byte
     * AccountIdentity seed (IDENTITY — always present when this
     * object is non-null); the session token pair is ACCESS and is
     * `null` when the grant redeem failed (seed-only degradation).
     * The consumer (AuthRepository.completeFromPairing /
     * completeFromDeviceRestore) owns zeroizing `phraseSeed`.
     */
    public class JoinRecoveryOutcome(
        public val phraseSeed: ByteArray,
        public val sessionToken: String?,
        public val sessionExpiresAtMs: Long?,
    )

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
         *
         * With `recovery = true` (ADR-0018 factor (a)) also polls
         * past the Welcome for the scanner's sealed recovery row,
         * opens it under the shared post-join exporter key, redeems
         * the grant for a session token, and returns the typed
         * outcome. Absence of the row (older scanner, or its
         * best-effort half failed) returns `null` — the join itself
         * is unaffected and the caller falls back to phrase entry.
         */
        public suspend fun accept(
            welcomeBytes: ByteArray,
            welcomeCursor: Long? = null,
            recovery: Boolean = false,
        ): JoinRecoveryOutcome? {
            check(!disposed) { "JoinSession.accept called after dispose" }
            // The deterministic group_id check inside
            // DeviceGroup.joinFromWelcome rejects a misrouted
            // Welcome before any state advance.
            val joined = DeviceGroup.joinFromWelcome(
                welcome = welcomeBytes,
                identity = identity,
                accountId = accountIdBytes,
            )
            group = joined
            DeviceGroupLifecycle.persistSnapshot(
                context = context,
                identity = identity,
                accountIdB64 = accountIdB64,
            )

            // Phase 2a3d8-ui — history catch-up bundle.
            if (welcomeCursor != null) {
                try {
                    val outcome = WelcomeCourier.pollForHistoryBundle(
                        rendezvousPseudonym = rendezvous,
                        welcomeCursor = welcomeCursor,
                    )
                    if (outcome is WelcomeCourier.HistoryBundleOutcome.Received) {
                        val secret = joined.historyBundleSecret(identity)
                        val applied = HistoryBundleCodec.applyToMessageLog(
                            context = context,
                            accountIdB64 = accountIdB64,
                            secret = secret,
                            sealedBytes = outcome.sealedBytes,
                        )
                        android.util.Log.i(
                            "PairingOrchestrator",
                            "history catch-up: applied $applied rows",
                        )
                    }
                } catch (cause: Throwable) {
                    android.util.Log.w(
                        "PairingOrchestrator",
                        "history bundle apply failed",
                        cause,
                    )
                }
            }

            // ADR-0018 factor (a) — opt-in recovery payload.
            if (recovery && welcomeCursor != null) {
                return runJoinerRecovery(joined, welcomeCursor)
            }
            return null
        }

        /**
         * Poll for the scanner's sealed recovery row, open it under
         * the shared post-join exporter key, redeem the grant.
         * Every failure degrades (row absent → `null`; redeem
         * failure → seed-only outcome) — the join is never broken
         * by this path. Transient secrets are zeroized.
         */
        private suspend fun runJoinerRecovery(
            joined: DeviceGroup,
            welcomeCursor: Long,
        ): JoinRecoveryOutcome? {
            val outcome = try {
                RecoveryPayload.pollForRecoveryPayload(
                    rendezvousPseudonym = rendezvous,
                    afterCursor = welcomeCursor,
                )
            } catch (cause: Throwable) {
                android.util.Log.w(
                    "PairingOrchestrator",
                    "recovery payload poll failed (join unaffected)",
                    cause,
                )
                return null
            }
            if (outcome !is RecoveryPayload.RecoveryPayloadOutcome.Received) {
                return null
            }
            var exporterKey: ByteArray? = null
            var payload: ByteArray? = null
            var grantToken: ByteArray? = null
            try {
                exporterKey = joined.exportSecret(
                    identity,
                    RecoveryPayload.RECOVERY_EXPORTER_LABEL,
                    rendezvous,
                    RecoveryPayload.RECOVERY_EXPORTER_KEY_LENGTH.toUInt(),
                )
                payload = openWithKey(exporterKey, outcome.sealedBlob)
                val parsed = RecoveryPayload.parseRecoveryPayload(payload)
                grantToken = parsed.grantToken
                // Redeem the single-use grant for a session token.
                // A redeem failure (endpoint down, grant expired)
                // degrades to a seed-only outcome — the identity
                // still restores; only ACCESS needs another factor.
                val redeemed = try {
                    AuthClient().use { client ->
                        client.deviceAuthorizeRedeem(
                            accountId = accountIdBytes,
                            grantTokenB64 = base64UrlEncodeNoPad(parsed.grantToken),
                        )
                    }
                } catch (cause: Throwable) {
                    android.util.Log.w(
                        "PairingOrchestrator",
                        "device grant redeem failed (seed-only outcome)",
                        cause,
                    )
                    null
                }
                return JoinRecoveryOutcome(
                    phraseSeed = parsed.phraseSeed,
                    sessionToken = redeemed?.sessionToken,
                    sessionExpiresAtMs = redeemed?.sessionExpiresAtMs,
                )
            } catch (cause: Throwable) {
                android.util.Log.w(
                    "PairingOrchestrator",
                    "recovery payload open failed (join unaffected)",
                    cause,
                )
                return null
            } finally {
                exporterKey?.fill(0)
                payload?.fill(0)
                grantToken?.fill(0)
            }
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
    /**
     * ADR-0018 factor (a) — inputs for the best-effort recovery
     * payload the scanner publishes alongside the Welcome, so the
     * fresh device gets ACCESS (a single-use grant) + IDENTITY (the
     * phrase seed) with no password or phrase entry.
     */
    public class ScanRecovery(
        /** Defensive copy of the 64-byte account-identity (phrase)
         *  seed; consumed and zeroized by the scan flow. */
        internal val accountIdentitySeed: ByteArray,
        /** Mints a single-use device grant
         *  (`AuthClient.deviceAuthorizeStart`, bearer + PoW);
         *  returns the URL-safe-base64 raw grant token. */
        internal val deviceAuthorizeStart: suspend () -> String,
    )

    public suspend fun runScanFlow(
        context: Context,
        scannedBytes: ByteArray,
        accountIdBytes: ByteArray,
        accountIdB64: String,
        recovery: ScanRecovery? = null,
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

            // G-7 — persist the advanced DeviceGroup snapshot
            // BEFORE any publish. addDeviceFromWire already merged
            // the pending commit, so the in-memory view is one
            // epoch ahead; writing it to disk first closes the
            // window where a crash between the publishes and the
            // persist leaves the on-disk envelope an epoch behind
            // (which would make the next launch try to add the
            // same device twice).
            DeviceGroupLifecycle.persistSnapshot(
                context = context,
                identity = ensured.identity,
                accountIdB64 = accountIdB64,
            )

            WelcomeCourier.publishWelcome(
                rendezvousPseudonym = decoded.rendezvousPseudonym,
                welcomeBytes = addition.welcome,
            )

            // G-5 — broadcast the Commit to the DeviceGroup DS
            // journal so any OTHER already-paired devices on this
            // account advance to the new epoch; their DeviceGroup
            // poll loop runs processIncoming on this row. MLS
            // requires in-order epoch processing, so this is on the
            // critical path (not best-effort) — a missed commit
            // would permanently strand a third device. For the
            // founder + first-pair case there is no other member,
            // so the row is a harmless no-op (the joiner ratchets
            // forward from the Welcome). Mirrors the
            // DeviceRemoval.removeDeviceAt broadcast.
            // G-13 (ADR-0015): every DeviceGroup journal entry is wrapped; an
            // add-device commit carries no attestation.
            val envelope = DeviceGroupJournalEnvelope.encode(addition.commit, null)
            DsClient().use { ds ->
                ds.publishGroup(
                    groupId = ensured.group.groupId(),
                    payload = envelope,
                )
            }

            // Phase 2a3d8-ui — best-effort history catch-up
            // bundle. Read the local message log, derive the
            // post-add exporter secret, seal, publish as the
            // *second* mailbox row. Any failure here is
            // non-fatal — the joining device still receives
            // the Welcome and ratchets forward.
            try {
                val secret = ensured.group.historyBundleSecret(
                    identity = ensured.identity,
                )
                val sealed = HistoryBundleCodec.buildFromMessageLog(
                    context = context,
                    accountIdB64 = accountIdB64,
                    secret = secret,
                )
                if (sealed != null) {
                    WelcomeCourier.publishHistoryBundle(
                        rendezvousPseudonym = decoded.rendezvousPseudonym,
                        sealedBytes = sealed,
                    )
                }
            } catch (cause: Throwable) {
                android.util.Log.w(
                    "PairingOrchestrator",
                    "history bundle publish failed",
                    cause,
                )
            }

            // ADR-0018 factor (a) — best-effort recovery payload.
            // Mint a single-use device grant, derive the exporter key
            // at the POST-ADD epoch (both devices share it after the
            // joiner processes the Welcome), seal
            // (grant ‖ account-identity seed) and publish the tagged
            // rendezvous row. Any failure is non-fatal: the pairing
            // completes and the joiner falls back to phrase entry.
            // The server never sees the seed (it travels only inside
            // the AEAD blob — invariant #2); transient copies are
            // zeroized below.
            if (recovery != null) {
                var grant: ByteArray? = null
                var exporterKey: ByteArray? = null
                var payload: ByteArray? = null
                try {
                    grant = base64UrlDecodeNoPad(recovery.deviceAuthorizeStart())
                    exporterKey = ensured.group.exportSecret(
                        ensured.identity,
                        RecoveryPayload.RECOVERY_EXPORTER_LABEL,
                        decoded.rendezvousPseudonym,
                        RecoveryPayload.RECOVERY_EXPORTER_KEY_LENGTH.toUInt(),
                    )
                    payload = RecoveryPayload.buildRecoveryPayload(
                        grantToken = grant,
                        phraseSeed = recovery.accountIdentitySeed,
                    )
                    val sealed = sealWithKey(exporterKey, payload)
                    RecoveryPayload.publishRecoveryPayload(
                        rendezvousPseudonym = decoded.rendezvousPseudonym,
                        sealedBlob = sealed,
                    )
                } catch (cause: Throwable) {
                    android.util.Log.w(
                        "PairingOrchestrator",
                        "recovery payload publish failed (pairing unaffected)",
                        cause,
                    )
                } finally {
                    grant?.fill(0)
                    exporterKey?.fill(0)
                    payload?.fill(0)
                }
            }

            return ScanResult(memberCount = if (ensured.bootstrapped) 2 else 0)
        } finally {
            recovery?.accountIdentitySeed?.fill(0)
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
