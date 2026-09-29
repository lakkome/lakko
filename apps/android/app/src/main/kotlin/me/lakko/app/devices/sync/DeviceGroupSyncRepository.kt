// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import android.content.Context
import android.util.Log
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import me.lakko.app.auth.IdentitySecrets
import me.lakko.app.auth.base64Encode
import me.lakko.app.devices.DeviceAttestKey
import me.lakko.app.devices.DeviceGroupJournalEnvelope
import me.lakko.app.devices.DeviceGroupLifecycle
import me.lakko.app.devices.DeviceGroupSnapshotStore
import me.lakko.app.devices.DeviceGroupJournalAccess
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsJournalSigner
import me.lakko.app.ds.DsSignerRegistry
import me.lakko.app.ds.DsSignerSource
import me.lakko.app.ds.dsGroupKey
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsNetworkError
import uniffi.lakko_ffi_uniffi.DeviceGroup
import uniffi.lakko_ffi_uniffi.DeviceGroupBody
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.ProcessedMessage
import uniffi.lakko_ffi_uniffi.parseDevicegroupPayload

/**
 * Cross-device sync runtime on Android (Phase 2a3e2b).
 *
 * Mirror of:
 *
 *   * `apps/web/src/devices/sync/DeviceGroupSyncContext.tsx`
 *   * `apps/ios/Lakko/Devices/Sync/DeviceGroupSyncRepository.swift`
 *
 * Holds the per-session DeviceGroup handle + four reactive
 * state maps (drafts / flags / readCursors / order) the UI
 * observes via [StateFlow], plus a background poll loop that
 * drains the DeviceGroup DS journal and a set of typed send
 * paths for the outbound side.
 *
 * Lifecycle:
 *
 *   * [bootstrap] (idempotent for the same `accountId`) —
 *     loads the DeviceGroup handle from the existing snapshot
 *     or bootstraps a founder leaf, kicks off the poll loop
 *     on the repository's [scope].
 *   * [clear] — cancels the poll loop coroutine, drops the
 *     handle, clears every reactive map. Called from
 *     `AuthRepository.logout` / `AuthRepository.forgetDevice`.
 */
public class DeviceGroupSyncRepository(
    private val applicationContext: Context,
) {

    // ------------------------------------------------------------
    // Reactive state
    // ------------------------------------------------------------

    private val mutableReady: MutableStateFlow<Boolean> =
        MutableStateFlow(false)
    public val ready: StateFlow<Boolean> = mutableReady.asStateFlow()

    private val mutableDrafts: MutableStateFlow<Map<String, DraftState>> =
        MutableStateFlow(emptyMap())
    public val drafts: StateFlow<Map<String, DraftState>> =
        mutableDrafts.asStateFlow()

    private val mutableFlags: MutableStateFlow<Map<String, FlagsState>> =
        MutableStateFlow(emptyMap())
    public val flags: StateFlow<Map<String, FlagsState>> =
        mutableFlags.asStateFlow()

    private val mutableReadCursors:
        MutableStateFlow<Map<String, ReadCursorState>> =
        MutableStateFlow(emptyMap())
    public val readCursors: StateFlow<Map<String, ReadCursorState>> =
        mutableReadCursors.asStateFlow()

    private val mutableOrder: MutableStateFlow<OrderState?> =
        MutableStateFlow(null)
    public val order: StateFlow<OrderState?> = mutableOrder.asStateFlow()

    // ------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------

    private val scope: CoroutineScope =
        CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val identity = AtomicReference<Identity?>(null)
    private val group = AtomicReference<DeviceGroup?>(null)
    private val activeAccountIdB64 = AtomicReference<String?>(null)

    /** ADR-0057 — the DeviceGroup journal's root signer (every device of the
     *  account holds it), derived at bootstrap from the account seed. */
    @Volatile private var rootSigner: me.lakko.app.ds.DsStatementSigning? = null
    @Volatile private var pollJob: Job? = null

    /** The debounced snapshot writer, cancelled with the poll loop. */
    @Volatile private var persistJob: Job? = null

    /** How long a change waits before the snapshot is rewritten. */
    private val SYNC_STATE_PERSIST_DEBOUNCE_MS: Long = 500

    // ------------------------------------------------------------
    // Public API
    // ------------------------------------------------------------

    /**
     * Bootstrap the cross-device sync runtime. Idempotent for
     * the same account. Called from `MainActivity` /
     * `SessionRoot` whenever a fresh [IdentitySecrets] value
     * lands.
     */
    public fun bootstrap(secrets: IdentitySecrets) {
        val accountIdB64 = base64Encode(secrets.accountId)
        val existing = activeAccountIdB64.get()
        if (existing == accountIdB64) return
        if (existing != null) {
            cancelPollAndDropHandles()
        }
        activeAccountIdB64.set(accountIdB64)

        val seed = try {
            me.lakko.app.auth.DeviceIdentityStore.ensureDeviceIdentitySeed(
                context = applicationContext,
                accountIdB64 = accountIdB64,
            )
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "ensureDeviceIdentitySeed failed", cause)
            activeAccountIdB64.set(null)
            return
        }

        val ensured = try {
            DeviceGroupLifecycle.ensureDeviceGroup(
                context = applicationContext,
                accountIdBytes = secrets.accountId,
                accountIdB64 = accountIdB64,
                deviceIdentitySeed = seed,
            )
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "ensureDeviceGroup failed", cause)
            activeAccountIdB64.set(null)
            return
        }
        seed.fill(0)

        identity.set(ensured.identity)
        group.set(ensured.group)

        // Seed the four maps from this device's own disk BEFORE the poll
        // starts. They used to begin empty on every launch, and the journal
        // cannot refill them: the poll runs from cursor 0, but the MLS
        // snapshot was restored above, so every row it re-fetches is one this
        // state has already consumed. The snapshot is a head start, not an
        // authority - the poll folds newer rows on top through the same HLC
        // comparison, so a sibling device's later write still wins.
        runCatching {
            DeviceGroupSnapshotStore.getSnapshot(
                context = applicationContext,
                accountIdB64 = accountIdB64,
                dir = DeviceGroupSnapshotStore.SYNC_STATE_DIR,
            )?.let(DeviceGroupSyncStateCodec::decode)
        }.getOrNull()?.let { restored ->
            mutableDrafts.value = restored.drafts
            mutableFlags.value = restored.flags
            mutableReadCursors.value = restored.readCursors
            mutableOrder.value = restored.order
        }
        startSyncStatePersistence(accountIdB64)
        mutableReady.value = true

        val groupId = try {
            ensured.group.groupId()
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "DeviceGroup.groupId failed", cause)
            return
        }
        // ADR-0053 — sign this journal's requests from every DsClient in the
        // process (the poll below, the send paths, the pairing / removal
        // publishes) and claim it: at founding this is the first registration,
        // on every later open an idempotent re-send.
        rootSigner = me.lakko.app.devices.DeviceGroupJournalAccess.rootOfSeed(secrets.accountIdentitySeed, groupId)
        installJournalSigner(groupId)
        pollJob = scope.launch {
            claimJournalBestEffort(groupId, refounded = true)
            runPollLoop(
                accountIdB64 = accountIdB64,
                groupId = groupId,
            )
        }
    }

    /**
     * Tear down. Cancels the poll loop, closes the FFI
     * handles, clears every reactive map. Idempotent.
     */
    public fun clear() {
        cancelPollAndDropHandles()
        // Forget the snapshot too. Logout and forget-this-device both come
        // through here, and a draft left behind for the next account to sign
        // in on this device is the one piece of this state that reads as a
        // message.
        activeAccountIdB64.get()?.let { closing ->
            runCatching {
                DeviceGroupSnapshotStore.deleteSnapshot(
                    context = applicationContext,
                    accountIdB64 = closing,
                    dir = DeviceGroupSnapshotStore.SYNC_STATE_DIR,
                )
            }
        }
        activeAccountIdB64.set(null)
        mutableReady.value = false
        mutableDrafts.value = emptyMap()
        mutableFlags.value = emptyMap()
        mutableReadCursors.value = emptyMap()
        mutableOrder.value = null
    }

    /**
     * Write the four maps to disk whenever any of them moves - a local set or
     * a sibling's row folded by the poll, since both land in these flows.
     *
     * Debounced because a draft moves on a keystroke and each write is an AES
     * seal plus a file replace. Best-effort: the in-memory state stays good
     * and the next change rewrites the envelope. The initial emission writes
     * back what bootstrap just read; harmless, and cheaper than a flag that
     * has to be cleared correctly on every path out of [bootstrap].
     */
    private fun startSyncStatePersistence(accountIdB64: String) {
        persistJob?.cancel()
        persistJob = scope.launch {
            combine(
                mutableDrafts,
                mutableFlags,
                mutableReadCursors,
                mutableOrder,
            ) { drafts, flags, cursors, order ->
                DeviceGroupSyncStateCodec.Snapshot(drafts, flags, cursors, order)
            }
                .debounce(SYNC_STATE_PERSIST_DEBOUNCE_MS)
                .collect { snapshot ->
                    val blob = DeviceGroupSyncStateCodec.encode(snapshot) ?: return@collect
                    runCatching {
                        DeviceGroupSnapshotStore.putSnapshot(
                            context = applicationContext,
                            accountIdB64 = accountIdB64,
                            snapshotBytes = blob,
                            dir = DeviceGroupSnapshotStore.SYNC_STATE_DIR,
                        )
                    }.onFailure { Log.w(LOG_TAG, "device-sync snapshot persist failed", it) }
                }
        }
    }

    /** Permanently destroy the repository — cancels the
     *  per-instance coroutine scope. After this, [bootstrap]
     *  no longer fires the poll loop. */
    public fun close() {
        clear()
        scope.cancel()
    }

    /** Lookup helper: current draft for `groupId` (raw bytes)
     *  or `null`. */
    public fun draftFor(groupId: ByteArray): DraftState? =
        drafts.value[groupIdHexOf(groupId)]

    /** Lookup helper: current flags or `null`. */
    public fun flagsFor(groupId: ByteArray): FlagsState? =
        flags.value[groupIdHexOf(groupId)]

    /** Lookup helper: current read cursor or `null`. */
    public fun readCursorFor(groupId: ByteArray): ReadCursorState? =
        readCursors.value[groupIdHexOf(groupId)]

    // ------------------------------------------------------------
    // Outbound send paths
    // ------------------------------------------------------------

    /** Compose + publish a `Draft` payload. Empty `text`
     *  clears the receivers' draft for the same group. */
    public suspend fun setDraft(
        groupId: ByteArray,
        text: String,
        replyToId: ByteArray = ByteArray(0),
    ) {
        val hlc = Hlc.mint()
        val body = DeviceGroupBody.Draft(
            groupId = groupId,
            text = text,
            replyToId = replyToId,
        )
        sendBody(body, hlc)
        val key = groupIdHexOf(groupId)
        mutableDrafts.value = mutableDrafts.value + (key to DraftState(
            text = text,
            replyToId = replyToId,
            updatedAtHlc = hlc,
        ))
    }

    /** Compose + publish a `Flags` payload. */
    public suspend fun setFlags(
        groupId: ByteArray,
        pinned: Boolean,
        muted: Boolean,
        archived: Boolean,
        unreadOverride: Boolean,
    ) {
        val hlc = Hlc.mint()
        val body = DeviceGroupBody.Flags(
            groupId = groupId,
            pinned = pinned,
            muted = muted,
            archived = archived,
            unreadOverride = unreadOverride,
        )
        sendBody(body, hlc)
        val key = groupIdHexOf(groupId)
        mutableFlags.value = mutableFlags.value + (key to FlagsState(
            pinned = pinned,
            muted = muted,
            archived = archived,
            unreadOverride = unreadOverride,
            updatedAtHlc = hlc,
        ))
    }

    /** Compose + publish a `ReadCursor` payload. */
    public suspend fun setReadCursor(
        groupId: ByteArray,
        upToMessageId: ByteArray,
        readAtMs: ULong,
    ) {
        val hlc = Hlc.mint()
        val body = DeviceGroupBody.ReadCursor(
            groupId = groupId,
            upToMessageId = upToMessageId,
            readAtMs = readAtMs,
        )
        sendBody(body, hlc)
        val key = groupIdHexOf(groupId)
        mutableReadCursors.value = mutableReadCursors.value + (key to ReadCursorState(
            upToMessageId = upToMessageId,
            readAtMs = readAtMs,
            updatedAtHlc = hlc,
        ))
    }

    /** Compose + publish an `Order` payload. */
    public suspend fun setOrder(groupIds: List<ByteArray>) {
        val hlc = Hlc.mint()
        val body = DeviceGroupBody.Order(groupIds = groupIds)
        sendBody(body, hlc)
        mutableOrder.value = OrderState(
            groupIds = groupIds,
            updatedAtHlc = hlc,
        )
    }

    private suspend fun sendBody(body: DeviceGroupBody, hlc: ULong) {
        val g = group.get() ?: error("DeviceGroupSync not bootstrapped")
        val id = identity.get() ?: error("DeviceGroupSync not bootstrapped")
        val accountIdB64 = activeAccountIdB64.get()
            ?: error("DeviceGroupSync not bootstrapped")
        val wireBytes = g.createDevicegroupMessage(id, body, hlc)
        val groupId = g.groupId()
        // G-13 (ADR-0015): wrap in the journal envelope (application messages
        // carry no attestation).
        val envelope = DeviceGroupJournalEnvelope.encode(wireBytes, null)
        DsClient().use { ds ->
            ds.publishGroup(groupId, envelope)
        }
        DeviceGroupLifecycle.persistSnapshot(
            context = applicationContext,
            identity = id,
            accountIdB64 = accountIdB64,
        )
    }

    // ------------------------------------------------------------
    // Poll loop
    // ------------------------------------------------------------

    private suspend fun runPollLoop(
        accountIdB64: String,
        groupId: ByteArray,
    ) {
        var cursor = 0L
        DsClient().use { ds ->
            while (currentCoroutineContextActive()) {
                val result = try {
                    ds.fetchGroup(
                        groupId = groupId,
                        after = cursor,
                        limit = null,
                        waitSecs = 25,
                    )
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (http: DsHttpError) {
                    if (http.status == 404) continue
                    Log.w(LOG_TAG, "poll fetch http", http)
                    delay(5000)
                    continue
                } catch (_: DsNetworkError) {
                    delay(2000)
                    continue
                } catch (cause: Throwable) {
                    Log.w(LOG_TAG, "poll fetch failed", cause)
                    delay(5000)
                    continue
                }

                // G-13 (ADR-0015): the 32-byte verifying key peers feed to
                // processIncomingVerifyingRemovals. A zero key is a fail-closed
                // fallback when the device-attest endpoint is unreachable — it
                // parses but verifies nothing, so a removal the policy requires
                // to be attested is refused rather than applied unverified;
                // while the policy is off, entries apply normally regardless.
                val attestVk: ByteArray = try {
                    DeviceAttestKey.fetch()
                } catch (cause: Throwable) {
                    Log.w(LOG_TAG, "device-attest key fetch failed", cause)
                    ByteArray(32)
                }

                var mutated = false
                for (message in result.messages) {
                    cursor = message.id
                    val g = group.get() ?: return
                    val id = identity.get() ?: return
                    // Unwrap the journal envelope (MLS message + any attestation).
                    val entry = try {
                        DeviceGroupJournalEnvelope.decode(message.payload)
                    } catch (cause: Throwable) {
                        Log.w(LOG_TAG, "journal envelope decode failed at ${message.id}", cause)
                        continue
                    }
                    val attestations: List<ByteArray> =
                        entry.second?.let { listOf(it) } ?: emptyList()
                    val processed: ProcessedMessage = try {
                        g.processIncomingVerifyingRemovals(
                            id,
                            entry.first,
                            attestations,
                            attestVk,
                            System.currentTimeMillis() / 1000,
                        )
                    } catch (cause: Throwable) {
                        Log.w(LOG_TAG, "processIncomingVerifyingRemovals failed at ${message.id}", cause)
                        continue
                    }
                    mutated = true
                    if (processed is ProcessedMessage.Application) {
                        try {
                            val parsed = parseDevicegroupPayload(
                                processed.`data`,
                            )
                            applyParsed(parsed)
                        } catch (cause: Throwable) {
                            Log.w(LOG_TAG, "parseDevicegroupPayload failed", cause)
                        }
                    }
                    // Commit / Proposal / ExternalJoinProposal —
                    // no application-state mutation.
                }

                if (mutated) {
                    val id = identity.get()
                    if (id != null) {
                        DeviceGroupLifecycle.persistSnapshot(
                            context = applicationContext,
                            identity = id,
                            accountIdB64 = accountIdB64,
                        )
                    }
                }
            }
        }
    }

    private fun applyParsed(
        parsed: uniffi.lakko_ffi_uniffi.ParsedDeviceGroupPayload,
    ) {
        val hlc = parsed.hlc
        when (val body = parsed.body) {
            is DeviceGroupBody.ReadCursor -> {
                val key = groupIdHexOf(body.groupId)
                val cur = mutableReadCursors.value[key]
                if (shouldApply(hlc, cur?.updatedAtHlc)) {
                    mutableReadCursors.value = mutableReadCursors.value + (
                        key to ReadCursorState(
                            upToMessageId = body.upToMessageId,
                            readAtMs = body.readAtMs,
                            updatedAtHlc = hlc,
                        )
                    )
                }
            }
            is DeviceGroupBody.Draft -> {
                val key = groupIdHexOf(body.groupId)
                val cur = mutableDrafts.value[key]
                if (shouldApply(hlc, cur?.updatedAtHlc)) {
                    mutableDrafts.value = mutableDrafts.value + (
                        key to DraftState(
                            text = body.text,
                            replyToId = body.replyToId,
                            updatedAtHlc = hlc,
                        )
                    )
                }
            }
            is DeviceGroupBody.Flags -> {
                val key = groupIdHexOf(body.groupId)
                val cur = mutableFlags.value[key]
                if (shouldApply(hlc, cur?.updatedAtHlc)) {
                    mutableFlags.value = mutableFlags.value + (
                        key to FlagsState(
                            pinned = body.pinned,
                            muted = body.muted,
                            archived = body.archived,
                            unreadOverride = body.unreadOverride,
                            updatedAtHlc = hlc,
                        )
                    )
                }
            }
            is DeviceGroupBody.Order -> {
                val cur = mutableOrder.value
                if (shouldApply(hlc, cur?.updatedAtHlc)) {
                    mutableOrder.value = OrderState(
                        groupIds = body.groupIds,
                        updatedAtHlc = hlc,
                    )
                }
            }
        }
    }

    /** ADR-0053 — install the DeviceGroup journal's signer source: derived
     *  from whatever handle this repository holds now. */
    private fun installJournalSigner(groupId: ByteArray) {
        DsSignerRegistry.shared.install(
            dsGroupKey(groupId),
            object : DsSignerSource {
                // The FFI DeviceGroup has no epoch read: derive every time.
                override fun epoch(): ULong? = null

                override val epochReadable: Boolean get() = false

                override fun derive(): DsJournalSigner? {
                    val g = group.get() ?: return null
                    val id = identity.get() ?: return null
                    return DeviceGroupJournalAccess.signerOf(g, id)
                }

                // ADR-0057 — every device of the account OWNS the DeviceGroup
                // journal, so a signed request's 401 may run the root claim.
                override val ownerClaim: (suspend () -> Boolean)?
                    get() = if (rootSigner == null) {
                        null
                    } else {
                        { claimJournalBestEffort(groupId, refounded = false) }
                    }
            },
            owner = DeviceGroupJournalAccess.REGISTRY_OWNER,
        )
    }

    /**
     * ADR-0057 obligations 1, 2 and 7 — claim the DeviceGroup journal with the
     * account's ROOT at the current epoch, best effort. At founding this is
     * the first claim; on every later open an idempotent re-send. A `409` on a
     * group this device just FOUNDED ([refounded]: no snapshot was found, so it
     * re-created the DeviceGroup under its deterministic id after lost state)
     * is a root RESET of the chain the gone group left; on a restored group a
     * `409` is left alone.
     */
    private suspend fun claimJournalBestEffort(groupId: ByteArray, refounded: Boolean): Boolean {
        val key = dsGroupKey(groupId)
        val signer = DsSignerRegistry.shared.current(key) ?: return false
        val root = rootSigner ?: return false
        val accountIdB64 = activeAccountIdB64.get()
        // ADR-0057 finding 3 — a reset is only ever for a founding this device
        // has not confirmed yet (lost state re-created the DeviceGroup under
        // its deterministic id); a confirmed group's 409 is left alone.
        val unconfirmed = refounded && accountIdB64 != null &&
            me.lakko.app.devices.DeviceGroupJournalAccess.isFoundingUnconfirmed(applicationContext, accountIdB64)
        return try {
            DsClient().use { ds ->
                val outcome = me.lakko.app.ds.claimOrReset(
                    journalId = groupId,
                    epoch = signer.epoch,
                    epochKey = signer.verifyingKey,
                    statements = root,
                    refounded = unconfirmed,
                    put = { make -> ds.putStatement(make) },
                )
                Log.i(LOG_TAG, "DeviceGroup journal claim: $outcome")
                val landed = outcome != me.lakko.app.ds.JournalClaimOutcome.ALREADY_CLAIMED
                if (landed && accountIdB64 != null) {
                    me.lakko.app.devices.DeviceGroupJournalAccess.clearFoundingUnconfirmed(applicationContext, accountIdB64)
                }
                landed
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "DeviceGroup journal claim did not land", cause)
            false
        }
    }

    private fun cancelPollAndDropHandles() {
        persistJob?.cancel()
        persistJob = null
        pollJob?.cancel()
        pollJob = null
        // Drop wasm handle references. We don't `.close()` them
        // — the UniFFI Object pattern means closing here while
        // some other call path still holds a reference would
        // crash. Letting them go out of scope is enough; the
        // JNA finalizer reclaims the Rust-side Arc.
        identity.set(null)
        group.set(null)
        rootSigner = null
        DsSignerRegistry.shared.clearOwner(DeviceGroupJournalAccess.REGISTRY_OWNER)
    }

    private companion object {
        const val LOG_TAG = "DeviceGroupSync"
    }
}

/**
 * Tiny wrapper around `currentCoroutineContext().isActive`
 * that doesn't require importing
 * `kotlinx.coroutines.currentCoroutineContext` at every call
 * site — same shape as the WelcomeCourier helper.
 */
private suspend fun currentCoroutineContextActive(): Boolean =
    kotlinx.coroutines.currentCoroutineContext().isActive
