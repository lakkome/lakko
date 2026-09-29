// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.Context
import me.lakko.app.auth.AuthClient
import me.lakko.app.auth.DeviceIdentityStore
import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64Encode
import me.lakko.app.ds.DsClient
import java.security.SecureRandom
import uniffi.lakko_ffi_uniffi.DecodedPairingQr
import uniffi.lakko_ffi_uniffi.DeviceGroup
import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.deviceGroupIdFor
import uniffi.lakko_ffi_uniffi.loadDeviceGroup
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
 *     function: take a payload already checked against this
 *     account's login handle ([decodeForOwnAccount]), add the new
 *     device's leaf, publish the Welcome, persist the snapshot.
 */
public object PairingOrchestrator {

    /**
     * ADR-0018 factor (a) — what the joiner recovered from the
     * scanner's sealed rendezvous row. `phraseSeed` is the 64-byte
     * AccountIdentity seed (IDENTITY — always present when this
     * object is non-null); the session token pair is ACCESS and is
     * `null` when the grant redeem failed (seed-only degradation).
     * `accountId` is the raw 16-byte account id the redeem response
     * named (ADR-0056: the only place a device that joined by name
     * learns it), `null` exactly when the redeem failed.
     * The consumer (AuthRepository.completeFromPairing /
     * completeFromDeviceRestore) owns zeroizing `phraseSeed`.
     *
     * `persistJoin` (ADR-0056 security pass) is set by a join by name: it
     * holds everything the join received (the DeviceIdentity seed, the
     * DeviceGroup snapshot, the history rows, the state bundle) and writes
     * it. The restore runs it only after the phrase seed verified against
     * the published identity AND the typed name resolved, at discovery, to
     * the redeemed account. `null` for a join that knew its account (it
     * persisted at join time, as before).
     */
    public class JoinRecoveryOutcome(
        public val phraseSeed: ByteArray,
        public val sessionToken: String?,
        public val sessionExpiresAtMs: Long?,
        public val accountId: ByteArray?,
        public val persistJoin: (suspend () -> Unit)? = null,
    )

    /**
     * An account this device already knows (it holds a session or a
     * parked login for it). A join for a known account checks the
     * Welcome against its DeviceGroup id up front, as before ADR-0056.
     */
    public class KnownAccount(
        public val accountIdBytes: ByteArray,
        public val accountIdB64: String,
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
     * Two shapes (ADR-0056):
     *
     *   * **known account** — `known` is set; the DeviceIdentity
     *     seed is the account's persisted one and the Welcome is
     *     joined as that account's DeviceGroup.
     *   * **by name** — `known` is `null`; the device has no
     *     account id and must not look one up. The DeviceIdentity
     *     seed lives in memory only, the Welcome is joined as a
     *     plain group, and nothing is persisted until the grant
     *     redeem names the account and that account's DeviceGroup
     *     id matches the joined group. This shape requires the
     *     recovery row (`accept(recovery = true)`).
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
        private val known: KnownAccount?,
        /** By-name only: the in-memory DeviceIdentity seed, persisted
         *  once the account is known, zeroized on dispose. */
        private val unboundDeviceSeed: ByteArray?,
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
         * For a known account, throws when the embedded
         * `group_id` doesn't match the deterministic
         * `BLAKE3("lakko-device-group/v1" || account_id)` value —
         * defensive against a misrouted Welcome from a different
         * user. For a join by name the same check runs after the
         * redeem names the account (see [JoinSession]).
         *
         * With `recovery = true` (ADR-0018 factor (a)) also polls
         * past the Welcome for the scanner's sealed recovery row,
         * opens it under the shared post-join exporter key, redeems
         * the grant for a session token, and returns the typed
         * outcome. Absence of the row (older scanner, or its
         * best-effort half failed) returns `null` — for a known
         * account the join itself is unaffected and the caller
         * falls back to phrase entry; for a join by name nothing
         * was persisted.
         */
        public suspend fun accept(
            welcomeBytes: ByteArray,
            welcomeCursor: Long? = null,
            recovery: Boolean = false,
        ): JoinRecoveryOutcome? {
            check(!disposed) { "JoinSession.accept called after dispose" }
            val account = known
                ?: return acceptByName(welcomeBytes, welcomeCursor, recovery)
            // The deterministic group_id check inside
            // DeviceGroup.joinFromWelcome rejects a misrouted
            // Welcome before any state advance.
            val joined = DeviceGroup.joinFromWelcome(
                welcome = welcomeBytes,
                identity = identity,
                accountId = account.accountIdBytes,
            )
            group = joined
            // ADR-0050 — a device joining by pairing is admitted to the
            // account's chats by the scanning device; say so until they land.
            StateBundle.markRestored(context)
            DeviceGroupLifecycle.persistSnapshot(
                context = context,
                identity = identity,
                accountIdB64 = account.accountIdB64,
            )
            if (welcomeCursor != null) {
                applyCatchUpBundles(joined, account.accountIdB64, welcomeCursor)
            }

            // ADR-0018 factor (a) — opt-in recovery payload.
            if (recovery && welcomeCursor != null) {
                return runJoinerRecovery(joined, account, welcomeCursor)
            }
            return null
        }

        /**
         * ADR-0056 join by name: join the Welcome as a plain group,
         * open the recovery row, redeem the grant (grant only), and
         * only when the redeemed account's DeviceGroup id is the
         * joined group persist the DeviceIdentity seed and the
         * snapshot under that account.
         */
        private suspend fun acceptByName(
            welcomeBytes: ByteArray,
            welcomeCursor: Long?,
            recovery: Boolean,
        ): JoinRecoveryOutcome? {
            val seed = checkNotNull(unboundDeviceSeed) { "join by name without a device seed" }
            check(recovery && welcomeCursor != null) {
                "a join by name needs the recovery row: without it the account stays unknown"
            }
            // The plain-Group join: the DeviceGroup join would check the
            // Welcome against an account id this device does not have yet.
            // The exporter is the same one DeviceGroup.exportSecret
            // delegates to, so the recovery row opens under it.
            val plain = Group.joinFromWelcome(welcome = welcomeBytes, identity = identity)
            try {
                return runJoinByName(
                    object : JoinByNameSteps {
                        override suspend fun pollRecoveryRow(): ByteArray? {
                            val row = try {
                                RecoveryPayload.pollForRecoveryPayload(
                                    rendezvousPseudonym = rendezvous,
                                    afterCursor = welcomeCursor,
                                )
                            } catch (cause: kotlinx.coroutines.CancellationException) {
                                throw cause
                            } catch (cause: Throwable) {
                                android.util.Log.w("PairingOrchestrator", "recovery payload poll failed", cause)
                                return null
                            }
                            return (row as? RecoveryPayload.RecoveryPayloadOutcome.Received)?.sealedBlob
                        }

                        override fun openRecoveryRow(sealed: ByteArray): RecoveryPayload.ParsedRecoveryPayload {
                            var key: ByteArray? = null
                            var payload: ByteArray? = null
                            try {
                                key = plain.exportSecret(
                                    identity,
                                    RecoveryPayload.RECOVERY_EXPORTER_LABEL,
                                    rendezvous,
                                    RecoveryPayload.RECOVERY_EXPORTER_KEY_LENGTH.toUInt(),
                                )
                                payload = openWithKey(key, sealed)
                                return RecoveryPayload.parseRecoveryPayload(payload)
                            } finally {
                                key?.fill(0)
                                payload?.fill(0)
                            }
                        }

                        override fun joinedGroupId(): ByteArray = plain.groupId()

                        override suspend fun redeem(grantTokenB64: String) =
                            AuthClient().use { client -> client.deviceAuthorizeRedeem(grantTokenB64) }

                        override fun groupIdFor(accountId: ByteArray): ByteArray =
                            deviceGroupIdFor(accountId)

                        override suspend fun bindAs(accountId: ByteArray): PendingJoin {
                            // In-memory only: the DeviceGroup is in the
                            // identity's own storage after the join, and the
                            // two bundles are fetched, not applied.
                            val joined = loadDeviceGroup(identity, accountId)
                                ?: throw IllegalStateException("joined DeviceGroup not found in storage")
                            group = joined
                            val accountIdB64 = base64Encode(accountId)
                            val bundles = fetchCatchUpBundles(joined, welcomeCursor)
                            return object : PendingJoin {
                                private var written = false

                                override suspend fun persist() {
                                    if (written) return
                                    written = true
                                    DeviceIdentityStore.putDeviceIdentitySeed(context, accountIdB64, seed)
                                    // ADR-0050 — the scanning device admits this one to
                                    // the account's chats; say so until they land.
                                    StateBundle.markRestored(context)
                                    DeviceGroupLifecycle.persistSnapshot(
                                        context = context,
                                        identity = identity,
                                        accountIdB64 = accountIdB64,
                                    )
                                    applyFetchedBundles(joined, accountIdB64, bundles)
                                }
                            }
                        }
                    },
                )
            } finally {
                try {
                    plain.close()
                } catch (_: Throwable) {
                    // best-effort
                }
            }
        }

        /** The two catch-up bundles of a join by name, fetched and held
         *  until the restore has passed its checks (ADR-0056 security pass). */
        private class FetchedBundles(
            val historySealed: ByteArray?,
            val stateBundle: StateBundle.Bundle?,
        )

        /** Fetch (not apply) the history and state bundles. Best-effort. */
        private suspend fun fetchCatchUpBundles(joined: DeviceGroup, welcomeCursor: Long): FetchedBundles {
            val history = try {
                val outcome = WelcomeCourier.pollForHistoryBundle(
                    rendezvousPseudonym = rendezvous,
                    welcomeCursor = welcomeCursor,
                )
                (outcome as? WelcomeCourier.HistoryBundleOutcome.Received)?.sealedBytes
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("PairingOrchestrator", "history bundle fetch failed", cause)
                null
            }
            var key: ByteArray? = null
            val state = try {
                val sealed = StateBundle.poll(rendezvous, welcomeCursor)
                if (sealed == null) {
                    null
                } else {
                    key = joined.exportSecret(
                        identity,
                        StateBundle.EXPORTER_LABEL,
                        rendezvous,
                        StateBundle.EXPORTER_KEY_LENGTH.toUInt(),
                    )
                    StateBundle.decode(openWithKey(key, sealed))
                }
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("PairingOrchestrator", "state bundle fetch failed", cause)
                null
            } finally {
                key?.fill(0)
            }
            return FetchedBundles(history, state)
        }

        /** Apply what [fetchCatchUpBundles] held. Best-effort. */
        private fun applyFetchedBundles(joined: DeviceGroup, accountIdB64: String, bundles: FetchedBundles) {
            val sealed = bundles.historySealed
            if (sealed != null) {
                try {
                    val secret = joined.historyBundleSecret(identity)
                    try {
                        val applied = HistoryBundleCodec.applyToMessageLog(
                            context = context,
                            accountIdB64 = accountIdB64,
                            secret = secret,
                            sealedBytes = sealed,
                        )
                        android.util.Log.i("PairingOrchestrator", "history catch-up: applied $applied rows")
                    } finally {
                        secret.fill(0)
                    }
                } catch (cause: Throwable) {
                    android.util.Log.w("PairingOrchestrator", "history bundle apply failed", cause)
                }
            }
            val bundle = bundles.stateBundle
            if (bundle != null) {
                try {
                    val (contacts, pins) = StateBundle.apply(context, bundle)
                    android.util.Log.i("PairingOrchestrator", "state bundle: $contacts contacts, $pins pins")
                } catch (cause: Throwable) {
                    android.util.Log.w("PairingOrchestrator", "state bundle apply failed", cause)
                }
            }
        }

        /** Phase 2a3d8-ui history catch-up + ADR-0050 section 2.9 state
         *  bundle, both best-effort. */
        private suspend fun applyCatchUpBundles(
            joined: DeviceGroup,
            accountIdB64: String,
            welcomeCursor: Long,
        ) {
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
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w(
                    "PairingOrchestrator",
                    "history bundle apply failed",
                    cause,
                )
            }

            var key: ByteArray? = null
            try {
                val sealed = StateBundle.poll(rendezvous, welcomeCursor)
                if (sealed != null) {
                    key = joined.exportSecret(
                        identity,
                        StateBundle.EXPORTER_LABEL,
                        rendezvous,
                        StateBundle.EXPORTER_KEY_LENGTH.toUInt(),
                    )
                    val bundle = StateBundle.decode(openWithKey(key, sealed))
                    if (bundle != null) {
                        val (contacts, pins) = StateBundle.apply(context, bundle)
                        android.util.Log.i(
                            "PairingOrchestrator",
                            "state bundle: $contacts contacts, $pins pins",
                        )
                    }
                }
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("PairingOrchestrator", "state bundle apply failed", cause)
            } finally {
                key?.fill(0)
            }
        }

        /**
         * Poll for the scanner's sealed recovery row, open it under
         * the shared post-join exporter key, redeem the grant.
         * Every failure degrades (row absent → `null`; redeem
         * failure, or a redeem naming another account → seed-only
         * outcome) — the join is never broken by this path.
         * Transient secrets are zeroized.
         */
        private suspend fun runJoinerRecovery(
            joined: DeviceGroup,
            account: KnownAccount,
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
                // Redeem the single-use grant for a session token (the
                // request carries the grant only, ADR-0056). A redeem
                // failure (endpoint down, grant expired), or a redeem that
                // names an account whose DeviceGroup is not the one just
                // joined, degrades to a seed-only outcome — the identity
                // still restores; only ACCESS needs another factor.
                val redeemed = completeJoinByName(
                    joinedGroupId = joined.groupId(),
                    grantToken = parsed.grantToken,
                    redeem = { grantB64 ->
                        AuthClient().use { client -> client.deviceAuthorizeRedeem(grantB64) }
                    },
                    groupIdFor = { accountId -> deviceGroupIdFor(accountId) },
                ) as? JoinByNameResult.Joined
                if (redeemed == null) {
                    android.util.Log.w(
                        "PairingOrchestrator",
                        "device grant redeem failed (seed-only outcome)",
                    )
                }
                return JoinRecoveryOutcome(
                    phraseSeed = parsed.phraseSeed,
                    sessionToken = redeemed?.sessionToken,
                    sessionExpiresAtMs = redeemed?.sessionExpiresAtMs,
                    accountId = redeemed?.accountId ?: account.accountIdBytes.copyOf(),
                )
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
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
            unboundDeviceSeed?.fill(0)
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
     * Joining-device side entry point. Builds the DeviceIdentity,
     * generates a fresh rendezvous slot, produces a KeyPackage,
     * encodes the pairing payload with `loginHandle`
     * (`loginHandle(username)` of the account's name, ADR-0056).
     * The view drives the rest via the returned [JoinSession].
     *
     * `known` is the account this device already knows (its
     * persisted DeviceIdentity seed is used); `null` is a join by
     * name, whose DeviceIdentity seed stays in memory until the
     * grant redeem names the account.
     *
     * The caller owns [JoinSession.dispose] — the typical
     * call site sits inside a Compose `DisposableEffect`
     * whose `onDispose` block invokes it.
     */
    public fun prepareJoinFlow(
        context: Context,
        loginHandle: ByteArray,
        known: KnownAccount?,
    ): JoinSession {
        val seed = if (known != null) {
            DeviceIdentityStore.ensureDeviceIdentitySeed(
                context = context,
                accountIdB64 = known.accountIdB64,
            )
        } else {
            ByteArray(DeviceIdentityStore.SEED_LENGTH).also { SecureRandom().nextBytes(it) }
        }
        val identity = try {
            Identity.fromSeed(seed)
        } catch (cause: Throwable) {
            seed.fill(0)
            throw cause
        }
        // Known account: best-effort scrub of the raw seed bytes; the
        // Identity has them folded into its in-memory state. By name:
        // the seed is kept (and zeroized on dispose) until the account
        // is known and it can be persisted.
        if (known != null) seed.fill(0)
        val keyPackage = identity.createKeyPackage()
        val rendezvous = PairingCodec.generateRendezvousPseudonym()
        val qrPayload = PairingCodec.buildPairingQrPayload(
            loginHandle = loginHandle,
            keyPackage = keyPackage,
            rendezvousPseudonym = rendezvous,
        )
        return JoinSession(
            qrPayload = qrPayload,
            rendezvous = rendezvous,
            identity = identity,
            known = known,
            unboundDeviceSeed = if (known == null) seed else null,
            context = context.applicationContext,
        )
    }

    /** Result of [runScanFlow]. */
    public data class ScanResult(
        /** Number of devices in the local DeviceGroup AFTER
         *  the add. Best-effort estimate today — full
         *  `devices()` accessor lands alongside 2a3c. */
        val memberCount: Int,
        /** ADR-0050 section 2.8 — the added device's 32-byte signature key:
         *  its DeviceGroup leaf, its conversation leaf and its discovery
         *  `device_id` are one key. The caller hands it to
         *  `ChatRepository.admitDevice`, which admits the device to every chat. */
        val newDeviceKey: ByteArray,
    )

    /**
     * Scanner-device side entry point. One-shot: run
     * `addDeviceFromWire` on a payload [decodeForOwnAccount]
     * already checked against this account's own login handle
     * (ADR-0056 — the check runs at scan time, before the
     * confirmation), publish the Welcome, persist the snapshot.
     *
     * Throws the inner error verbatim when:
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
         *  (`AuthClient.deviceAuthorizeStart`, bearer);
         *  returns the URL-safe-base64 raw grant token. */
        internal val deviceAuthorizeStart: suspend () -> String,
    )

    public suspend fun runScanFlow(
        context: Context,
        decoded: DecodedPairingQr,
        accountIdBytes: ByteArray,
        accountIdB64: String,
        recovery: ScanRecovery? = null,
    ): ScanResult {
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
            // ADR-0057 finding 3 — no Welcome for a DeviceGroup whose founding
            // claim has not landed: claim first, fail closed.
            DsClient().use { ds ->
                DeviceGroupJournalAccess.claimFoundingIfUnconfirmed(
                    context = context,
                    accountIdB64 = accountIdB64,
                    group = ensured.group,
                    identity = ensured.identity,
                    ds = ds,
                )
            }
            // ADR-0058 section 5.1 — the members before the add, so the sync
            // runtime can decide whether this device re-sends its state.
            val leavesBeforeAdd = runCatching {
                ensured.group.devices().map { it.leafIndex }
            }.getOrNull()
            val signerBeforeCommit = DeviceGroupJournalAccess.signerOf(ensured.group, ensured.identity)
            val addition = ensured.group.addDeviceFromWire(
                identity = ensured.identity,
                keyPackage = decoded.keyPackage,
            )
            // ADR-0053 client contract point 3 — register epoch N+1 BEFORE the
            // snapshot is persisted or anything published. A refusal throws out
            // of here with nothing durable and nothing on the wire: this handle
            // is throwaway, so the durable DeviceGroup stays at N.
            val commitSigner = DsClient().use { ds ->
                DeviceGroupJournalAccess.registerNext(
                    ds = ds,
                    groupId = ensured.group.groupId(),
                    before = signerBeforeCommit,
                    after = DeviceGroupJournalAccess.signerOf(ensured.group, ensured.identity),
                    root = DeviceGroupJournalAccess.rootOf(context, accountIdB64, ensured.group.groupId()),
                )
            }

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
                    signer = commitSigner,
                )
            }

            // ADR-0058 section 5.1 — this device never receives its own commit
            // back, so tell the sync runtime: it reopens its DeviceGroup handle
            // at the new epoch and, when it is the lowest prior leaf, re-sends
            // the folded state (folders, pins, drafts) the new device cannot
            // decrypt from the journal.
            if (leavesBeforeAdd != null) {
                me.lakko.app.devices.sync.DeviceGroupSyncRepository.active
                    ?.onLocalDeviceAdded(leavesBeforeAdd)
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

            // ADR-0050 section 2.9 — the state bundle: contacts, pins and the
            // ids of every group this device holds, sealed under the per-pairing
            // exporter like the recovery row. Best-effort; a failure leaves the
            // new device with the chats it is admitted to and no contact names.
            run {
                var key: ByteArray? = null
                try {
                    val known = me.lakko.app.chat.KnownGroupsStore(context).load(accountIdB64)
                    val bundle = StateBundle.build(context, known)
                    key = ensured.group.exportSecret(
                        ensured.identity,
                        StateBundle.EXPORTER_LABEL,
                        decoded.rendezvousPseudonym,
                        StateBundle.EXPORTER_KEY_LENGTH.toUInt(),
                    )
                    val sealed = sealWithKey(key, StateBundle.encode(bundle))
                    StateBundle.publish(decoded.rendezvousPseudonym, sealed)
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (cause: Throwable) {
                    android.util.Log.w("PairingOrchestrator", "state bundle publish failed", cause)
                } finally {
                    key?.fill(0)
                }
            }

            return ScanResult(
                memberCount = if (ensured.bootstrapped) 2 else 0,
                newDeviceKey = uniffi.lakko_ffi_uniffi.keyPackageSignatureKey(decoded.keyPackage),
            )
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
