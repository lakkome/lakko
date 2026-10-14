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
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import me.lakko.app.launchBound
import me.lakko.app.auth.IdentitySecrets
import me.lakko.app.auth.OwnUsernameState
import me.lakko.app.auth.RecoveryAddressState
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
import uniffi.lakko_ffi_uniffi.FolderChangeSet
import uniffi.lakko_ffi_uniffi.FolderView
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.ParsedDeviceGroupPayload
import uniffi.lakko_ffi_uniffi.ProcessedMessage
import uniffi.lakko_ffi_uniffi.SyncedState
import uniffi.lakko_ffi_uniffi.parseDevicegroupPayload
import uniffi.lakko_ffi_uniffi.shouldSendSnapshot

/**
 * Cross-device sync runtime on Android (Phase 2a3e2b).
 *
 * Mirror of:
 *
 *   * `apps/web/src/devices/sync/DeviceGroupSyncContext.tsx`
 *   * `apps/ios/Lakko/Devices/Sync/DeviceGroupSyncRepository.swift`
 *
 * Holds the per-session DeviceGroup handle and ONE folded
 * [SyncedState] (ADR-0058, `lakko-sync`): drafts, pin / mute /
 * archive flags, read cursors, the conversation order and the chat
 * folders. The fold itself is Rust, written once for three clients;
 * this class feeds it every payload the poll drains and every local
 * write, persists its bytes, and republishes the four maps the UI
 * already observed (plus [folders]) derived from it after every
 * change, so the consumers did not have to change.
 *
 * Lifecycle:
 *
 *   * [bootstrap] (idempotent for the same `accountId`) —
 *     loads the DeviceGroup handle from the existing snapshot
 *     or bootstraps a founder leaf, kicks off the poll loop
 *     on the repository's [scope].
 *   * [clear] — cancels the poll loop coroutine, drops the
 *     handle, clears every reactive map. Called on Log out and
 *     before Forget device / Delete account. It KEEPS the
 *     account's persisted folded state and its pending flag
 *     (writing out a change still waiting out its debounce);
 *     only [deleteAccountRows], from the account wipe, removes
 *     them.
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

    /** ADR-0058 — every live chat folder, in the resolved order. */
    private val mutableFolders: MutableStateFlow<List<FolderView>> =
        MutableStateFlow(emptyList())
    public val folders: StateFlow<List<FolderView>> = mutableFolders.asStateFlow()

    /**
     * UX spec chat-folders section 6 — something written here has not
     * reached the account's other devices yet. Drives the one quiet line in
     * Settings > Chat folders.
     */
    private val mutablePending: MutableStateFlow<Boolean> = MutableStateFlow(false)
    public val pendingSync: StateFlow<Boolean> = mutablePending.asStateFlow()

    /**
     * docs/ux/second-device.md section 3.1 — another device removed this one
     * from the account, and this device has applied that removal (its
     * DeviceGroup is inactive). The poll loop has stopped for good; the
     * signed-in root shows "This device was removed". Reset on sign-out and
     * on an account switch.
     */
    private val mutableDeviceRemoved: MutableStateFlow<Boolean> = MutableStateFlow(false)
    public val deviceRemoved: StateFlow<Boolean> = mutableDeviceRemoved.asStateFlow()

    /** Bumped on every change to the folded state; the persistence writer
     *  watches it. */
    private val stateVersion: MutableStateFlow<Long> = MutableStateFlow(0L)

    // ------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------

    private val scope: CoroutineScope =
        CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val identity = AtomicReference<Identity?>(null)
    private val group = AtomicReference<DeviceGroup?>(null)
    private val activeAccountIdB64 = AtomicReference<String?>(null)

    /**
     * The binding of this runtime to [activeAccountIdB64]: it ends when the
     * handles are dropped ([clear], a bootstrap for another account). The poll
     * loop asks it before every row and before its persist, because [identity]
     * and [group] are read live and would by then be the next account's.
     */
    private val sessionBinding = me.lakko.app.BindingEpoch()

    /** The binding current now (a host test reads when it ends). */
    internal fun captureSessionBinding(): me.lakko.app.Binding = sessionBinding.capture()

    /** ADR-0057 — the DeviceGroup journal's root signer (every device of the
     *  account holds it), derived at bootstrap from the account seed. */
    @Volatile private var rootSigner: me.lakko.app.ds.DsStatementSigning? = null
    private val activeAccountId = AtomicReference<ByteArray?>(null)
    @Volatile private var pollJob: Job? = null

    /** The publisher this runtime installed on the reach manager, so a
     *  teardown removes only its own. */
    @Volatile private var reachPublisher: ((ByteArray) -> Unit)? = null

    /** The account whose forked DeviceGroup this session already re-founded
     *  ([repairFork]); the repair runs at most once per session per account. */
    @Volatile private var forkRepairedFor: String? = null

    /** The folded state. Created lazily so constructing the repository never
     *  touches the native library. Every read-modify-republish runs under
     *  [stateLock] so the published maps always come from one state. */
    private val stateLock = Any()
    @Volatile private var syncedState: SyncedState? = null

    /** The member leaves this device last observed in the DeviceGroup, or
     *  `null` before the handle was read (ADR-0058 section 5.1). */
    @Volatile private var knownLeaves: Set<UInt>? = null

    @Volatile private var pendingFlag: PendingSyncFlag? = null

    /** How long a change waits before the snapshot is rewritten. */
    private val SYNC_STATE_PERSIST_DEBOUNCE_MS: Long = 500

    /**
     * The debounced snapshot writer ([startSyncStatePersistence]). Stopped
     * with the poll loop; the stop writes a change still waiting out its
     * debounce, so a teardown never loses one.
     */
    private val syncStatePersistence: SyncStatePersistence = SyncStatePersistence(
        scope = scope,
        versions = stateVersion,
        debounceMs = SYNC_STATE_PERSIST_DEBOUNCE_MS,
        encode = { currentSyncedStateBytes() },
        write = { accountIdB64, bytes ->
            DeviceGroupSnapshotStore.putSnapshot(
                context = applicationContext,
                accountIdB64 = accountIdB64,
                snapshotBytes = bytes,
                dir = DeviceGroupSnapshotStore.SYNC_STATE_DIR,
            )
        },
        onChanged = { accountIdB64 ->
            me.lakko.app.backup.AutoBackupStore.markDirty(
                applicationContext,
                accountIdB64,
                System.currentTimeMillis(),
            )
        },
    )

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
        active = this
        val accountIdB64 = base64Encode(secrets.accountId)
        val existing = activeAccountIdB64.get()
        if (existing == accountIdB64) return
        if (existing != null) {
            cancelPollAndDropHandles()
        }
        // An account switch: the removal belonged to the account left behind.
        mutableDeviceRemoved.value = false
        activeAccountIdB64.set(accountIdB64)
        activeAccountId.set(secrets.accountId.copyOf())
        openAndPoll(secrets.accountId, accountIdB64, seedState = true, accountIdentitySeed = secrets.accountIdentitySeed)
    }

    /**
     * Open the DeviceGroup handle from this device's disk and start the poll.
     * [seedState] loads the persisted folded state first; a reopen after a
     * local commit keeps the live one.
     */
    private fun openAndPoll(
        accountIdBytes: ByteArray,
        accountIdB64: String,
        seedState: Boolean,
        accountIdentitySeed: ByteArray? = null,
        startCursor: Long = 0L,
        /** This device just let a device in on a handle of its own (UX 17.5:
         *  no new-device notice on the device that let it in). */
        letIn: Boolean = false,
    ) {
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
                accountIdBytes = accountIdBytes,
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

        // Removed while the app was closed (or by the batch a reopen follows):
        // the persisted snapshot already carries the commit that removed this
        // device. Nothing is polled, folded or persisted for it; the root
        // shows the removed screen.
        if (RemovedDevice.isRemoved(RemovedDevice.readActive { ensured.group.isActive() })) {
            markRemoved()
            return
        }

        // Seed the folded state from this device's own disk BEFORE the poll
        // starts. It used to begin empty on every launch, and the journal
        // cannot refill it: the poll runs from cursor 0, but the MLS snapshot
        // was restored above, so every row it re-fetches is one this state
        // has already consumed. The snapshot is a head start, not an
        // authority - the poll folds newer rows on top, so a sibling device's
        // later write still wins. Bytes in an older format (encoding version
        // 1, before the saved-message register) do not decode and are
        // dropped: there is no migration (no production data). The state
        // starts empty, and the persistence writer's first write replaces the
        // refused file.
        if (seedState) {
            val restored = runCatching {
                DeviceGroupSnapshotStore.getSnapshot(
                    context = applicationContext,
                    accountIdB64 = accountIdB64,
                    dir = DeviceGroupSnapshotStore.SYNC_STATE_DIR,
                )?.let { SyncedState.fromBytes(it) }
            }.onFailure { Log.w(LOG_TAG, "persisted synced state dropped", it) }.getOrNull()
            synchronized(stateLock) {
                syncedState = restored ?: SyncedState()
            }
            val prefs = applicationContext.getSharedPreferences(PENDING_PREFS, Context.MODE_PRIVATE)
            pendingFlag = PendingSyncFlag(prefs.getBoolean(accountIdB64, false)) { value ->
                me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
                    prefs.edit().putBoolean(accountIdB64, value).apply()
                }
                mutablePending.value = value
            }.also { mutablePending.value = it.pending }
            republish()
        }
        knownLeaves = runCatching { ensured.group.devices().map { it.leafIndex }.toSet() }.getOrNull()
        // UX 17.5 - the new-device notice reads the account's device list off
        // this roster. A reopen after this device let a device in
        // acknowledges what is there: the device that let it in shows no
        // banner about it.
        me.lakko.app.devices.NewDeviceNotices.bind(applicationContext, accountIdB64)
        reportRoster(ensured.group, accountIdB64, localChange = letIn)
        if (seedState) startSyncStatePersistence(accountIdB64)
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
        // ADR-0057 — the root signer is derived at bootstrap; a reopen after a
        // local add ([onLocalDeviceAdded]) keeps the one it already holds.
        if (accountIdentitySeed != null) {
            rootSigner = me.lakko.app.devices.DeviceGroupJournalAccess.rootOfSeed(accountIdentitySeed, groupId)
        }
        installJournalSigner(groupId)
        installReachPublisher()
        val bound = sessionBinding.capture()
        pollJob = scope.launch {
            claimJournalBestEffort(groupId, refounded = true)
            val forkedAt = runPollLoop(
                bound = bound,
                accountIdB64 = accountIdB64,
                groupId = groupId,
                startCursor = startCursor,
            )
            if (forkedAt != null && bound.isCurrent) {
                repairFork(accountIdBytes, accountIdB64, forkedAt)
            }
        }
    }

    /**
     * The DeviceGroup this device holds forked from the chain on the DS
     * ([DeviceGroupFork]): it read the whole journal and is still behind the
     * newest registered epoch, which nothing that arrives will ever change.
     * Re-found it. The poll loop has already stopped.
     *
     * Only this account's DeviceGroup MLS snapshot is deleted. It lives in
     * its own store ([DeviceGroupSnapshotStore], its own folder and wrap key),
     * apart from the conversation groups (`IdentitySnapshotStore`); the folded
     * sync state in [DeviceGroupSnapshotStore.SYNC_STATE_DIR], the device
     * identity seed and everything else stay. Android keeps no stored
     * DeviceGroup cursor (every open polls from the cursor it is given).
     *
     * [openAndPoll] then finds no snapshot, so [DeviceGroupLifecycle] founds a
     * fresh DeviceGroup under the same deterministic id with the SAME device
     * identity (its conversation leaves in chats are unaffected) and marks the
     * founding unconfirmed, and [claimJournalBestEffort] claims the journal
     * with the account root and, on the `409` the old chain answers, RESETS
     * it (ADR-0057 obligation 7). If the claim or reset does not land, the
     * founding stays unconfirmed: no Welcome can leave for it
     * ([DeviceGroupJournalAccess.claimFoundingIfUnconfirmed]) and the next
     * open claims and resets again.
     *
     * Polling restarts at [headCursor], the head this device last saw: the
     * old group's rows cannot be read by the new one.
     *
     * Consequence: every OTHER device of the account loses the DeviceGroup.
     * The reset drops the chain their epoch keys were registered on, so their
     * reads answer 401, and each must be added again. A 401 is not a drained
     * poll, so none of them ever runs a repair of its own.
     */
    private fun repairFork(accountIdBytes: ByteArray, accountIdB64: String, headCursor: Long) {
        if (activeAccountIdB64.get() != accountIdB64) return
        // Never re-found the DeviceGroup of a removed device, nor of a handle
        // whose state cannot be read (docs/ux/second-device.md section 3.1).
        if (mutableDeviceRemoved.value) return
        val held = group.get() ?: return
        if (!RemovedDevice.forkRepairAllowed(RemovedDevice.readActive { held.isActive() })) return
        // Nothing may go out on the forked handle while the new one is made.
        group.set(null)
        identity.set(null)
        try {
            DeviceGroupSnapshotStore.deleteSnapshot(
                context = applicationContext,
                accountIdB64 = accountIdB64,
            )
        } catch (cause: Throwable) {
            // The old snapshot is then restored below and polling resumes on
            // it; the repair is not tried again this session.
            Log.w(LOG_TAG, "forked DeviceGroup snapshot not deleted (${cause.javaClass.simpleName})")
        }
        openAndPoll(accountIdBytes, accountIdB64, seedState = false, startCursor = headCursor)
    }

    /**
     * Tear down. Cancels the poll loop, closes the FFI
     * handles, clears every reactive map. Idempotent.
     *
     * Log out keeps the account's local data for its next sign-in, and this
     * state cannot be rebuilt from the journal (the restored DeviceGroup
     * snapshot has already consumed the rows that built it), so the persisted
     * folded state and the pending flag STAY: a change still waiting out its
     * debounce is written first, and nothing is written for the account after
     * this returns. The state is per-account, so the next account to sign in
     * on this device never reads it. Forget device and Delete account remove
     * both through [me.lakko.app.account.AccountLocalWipe] after this.
     */
    public fun clear() {
        me.lakko.app.devices.NewDeviceNotices.unbind()
        cancelPollAndDropHandles()
        activeAccountIdB64.set(null)
        activeAccountId.set(null)
        forkRepairedFor = null
        synchronized(stateLock) {
            syncedState = null
            mutableRecoveryEmail.value = RecoveryAddressState.NOT_READY
            mutableOwnUsername.value = OwnUsernameState.NOT_READY
        }
        pendingFlag = null
        knownLeaves = null
        mutableReady.value = false
        mutableDrafts.value = emptyMap()
        mutableFlags.value = emptyMap()
        mutableReadCursors.value = emptyMap()
        mutableOrder.value = null
        mutableFolders.value = emptyList()
        mutablePending.value = false
        mutableDeviceRemoved.value = false
    }

    /** Hand the DeviceGroup roster to the new-device notice (UX 17.5).
     *  Best-effort: a roster that cannot be read says nothing. */
    private fun reportRoster(g: DeviceGroup, accountIdB64: String, localChange: Boolean) {
        try {
            val own = g.ownLeafIndex()
            val rows = g.devices()
            val roster = rows.map { me.lakko.app.devices.newDeviceKey(it.signaturePublicKey) }.toSet()
            val self = rows.firstOrNull { it.leafIndex == own }
                ?.let { me.lakko.app.devices.newDeviceKey(it.signaturePublicKey) }
            if (localChange) {
                me.lakko.app.devices.NewDeviceNotices.onLetIn(accountIdB64, roster)
            } else {
                me.lakko.app.devices.NewDeviceNotices.onRoster(applicationContext, accountIdB64, roster, self)
            }
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "device roster not read for the new-device notice (${cause.javaClass.simpleName})")
        }
    }

    /**
     * ADR-0074 section 2.3 — the leaf signature keys of the devices in the
     * account's DeviceGroup, lowercase hex, as this device's live handle
     * holds them. `null` when there is no handle, the group is inactive
     * (this device was removed: its frozen roster proves nothing) or the
     * read fails. The conversation side admits a device of the own account
     * only when its key is in this set.
     */
    public fun deviceRosterHex(): Set<String>? {
        val held = group.get() ?: return null
        if (RemovedDevice.readActive { held.isActive() } != true) return null
        return try {
            held.devices().map { me.lakko.app.ds.DsClient.pseudonymHex(it.signaturePublicKey) }.toSet()
        } catch (cancellation: kotlinx.coroutines.CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        }
    }

    /**
     * This device was removed from the account (docs/ux/second-device.md
     * section 3.1): stop the poll loop for good this session and the
     * persistence writer, and publish [deviceRemoved]. The handles stay held
     * until [clear] drops them on sign-out; every send path refuses from now
     * on. Safe to call from inside the poll loop itself: the caller returns
     * right after, and the cancellation lands at its next suspension.
     */
    private fun markRemoved() {
        if (!mutableDeviceRemoved.value) {
            Log.w(LOG_TAG, "this device was removed from the account; sync stopped")
        }
        mutableDeviceRemoved.value = true
        syncStatePersistence.stopAndFlush()
        pollJob?.cancel()
        pollJob = null
    }

    /**
     * Write the folded state to disk whenever it moves - a local write or a
     * sibling's row folded by the poll, since both bump [stateVersion].
     *
     * Debounced because a draft moves on a keystroke and each write is an AES
     * seal plus a file replace. Best-effort: the in-memory state stays good
     * and the next change rewrites the envelope. Each write also marks the
     * account's backup dirty, since the backup carries these bytes (ADR-0058
     * section 5.2); the backup's own throttle bounds how often that uploads.
     */
    private fun startSyncStatePersistence(accountIdB64: String) {
        syncStatePersistence.start(accountIdB64)
    }

    /** The folded state's persistence bytes, or `null` before bootstrap.
     *  What the backup carries (ADR-0058 section 5.2). */
    public fun currentSyncedStateBytes(): ByteArray? = synchronized(stateLock) {
        val state = syncedState ?: return null
        runCatching { state.toBytes() }
            .onFailure { Log.w(LOG_TAG, "synced state encode failed", it) }
            .getOrNull()
    }

    /**
     * Re-derive every published map from the folded state. Called after any
     * change; the maps are small (one entry per conversation), and rebuilding
     * them whole keeps them exactly what the fold holds.
     */
    private fun republish() {
        synchronized(stateLock) {
            val state = syncedState ?: return
            try {
                republishRecoveryEmail(state)
                republishOwnUsername(state)
                republishLineage(state)
                // ADR-0073 R.5 - every person-facing store is keyed by
                // conversation id; a screen holding ANY segment's id reads the
                // conversation's row ([me.lakko.app.chat.aliasAcrossSegments]).
                val lineageEntries = mutableLineage.value
                val lineage = me.lakko.app.chat.buildLineage(lineageEntries)
                mutableDrafts.value = state.allDrafts().associate { d ->
                    groupIdHexOf(d.groupId) to DraftState(
                        text = d.text,
                        replyToId = d.replyToId,
                        updatedAtHlc = d.hlc,
                    )
                }.let { me.lakko.app.chat.aliasAcrossSegments(it, lineageEntries, lineage) }
                mutableFlags.value = state.allFlags().associate { f ->
                    groupIdHexOf(f.groupId) to FlagsState(
                        pinned = f.pinned,
                        muted = f.muted,
                        archived = f.archived,
                        unreadOverride = f.unreadOverride,
                        updatedAtHlc = f.hlc,
                    )
                }.let { me.lakko.app.chat.aliasAcrossSegments(it, lineageEntries, lineage) }
                val rawCursors = state.allReadCursors().associate { c ->
                    groupIdHexOf(c.groupId) to ReadCursorState(
                        upToMessageId = c.upToMessageId,
                        readAtMs = c.readAtMs,
                        updatedAtHlc = c.hlc,
                    )
                }
                conversationReadCursors = rawCursors
                // The read position is one per conversation and carries its
                // segment; each segment's readers get it as that segment's
                // own 8-byte message id.
                mutableReadCursors.value =
                    me.lakko.app.chat.aliasAcrossSegments(rawCursors, lineageEntries, lineage) { c, segment ->
                        me.lakko.app.chat.readCursorBytesForSegment(c.upToMessageId, segment)?.let { bytes ->
                            ReadCursorState(upToMessageId = bytes, readAtMs = c.readAtMs, updatedAtHlc = c.updatedAtHlc)
                        }
                    }
                mutableOrder.value = state.conversationOrder()?.let { o ->
                    OrderState(groupIds = o.groupIds, updatedAtHlc = o.hlc)
                }
                mutableFolders.value = state.folders()
                // A restored or received stamp may be ahead of this device's
                // clock; the next local write must still out-rank it.
                Hlc.advancePast(state.maxHlc())
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "synced state read failed", cause)
            }
            stateVersion.value = stateVersion.value + 1
        }
    }

    /** Fold one payload (received, or this device's own write) and
     *  republish when it changed anything. */
    private fun fold(payload: ParsedDeviceGroupPayload): Boolean {
        val changed = synchronized(stateLock) {
            val state = syncedState ?: return false
            try {
                state.applyPayload(payload)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "applyPayload refused", cause)
                false
            }
        }
        if (changed) republish()
        return changed
    }

    // ------------------------------------------------------------
    // ADR-0073 R.5 / R.13a - the conversation lineage
    // ------------------------------------------------------------

    private val mutableLineage: MutableStateFlow<List<me.lakko.app.chat.LineageEntry>> =
        MutableStateFlow(emptyList())

    /**
     * The lineage registers `successor group -> root conversation` of the
     * synced state, as plain values (lower-case hex ids). A conversation that
     * continues in a new group keeps its row, pin, mute, folder, draft and
     * read position through these: every person-facing store stays keyed on
     * the root.
     */
    public val lineage: StateFlow<List<me.lakko.app.chat.LineageEntry>> = mutableLineage.asStateFlow()

    /** Project the registers and mirror them for the readers that open no
     *  synced state ([me.lakko.app.chat.LineageMirror]). Under [stateLock]. */
    private fun republishLineage(state: SyncedState) {
        val entries = state.allLineage().map {
            me.lakko.app.chat.LineageEntry(
                successorHex = groupIdHexOf(it.successorGroupId),
                rootHex = groupIdHexOf(it.rootConversationId),
                hlc = it.hlc,
            )
        }
        if (entries == mutableLineage.value) return
        mutableLineage.value = entries
        activeAccountIdB64.get()?.let { me.lakko.app.chat.LineageMirror.save(applicationContext, it, entries) }
    }

    /**
     * Record that [successorGroupId] continues the conversation
     * [rootConversationId] and tell the account's other devices. WRITE-ONCE
     * on the writer's side: a successor this device already holds a register
     * for is left as it is (`false`). The register travels as a `lakko-sync`
     * lineage envelope inside a DeviceGroup `ServiceEnvelope` (R.13a); a
     * failed publish is not lost, since a catch-up re-send carries every
     * register again.
     */
    public suspend fun recordLineage(successorGroupId: ByteArray, rootConversationId: ByteArray): Boolean {
        val envelope = synchronized(stateLock) {
            val state = syncedState ?: return false
            val written = try {
                state.setLineage(Hlc.mint(), successorGroupId, rootConversationId)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "lineage register refused (${cause.javaClass.simpleName})")
                false
            }
            if (!written) return false
            runCatching { state.lineageEnvelopeFor(successorGroupId) }.getOrNull()
        }
        republish()
        if (envelope != null) {
            try {
                sendServiceEnvelope(envelope)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "lineage register not published (${cause.javaClass.simpleName})")
                pendingFlag?.recordFailure()
            }
        }
        return true
    }

    /** A received lineage envelope: folded by the ordinary rule, so two
     *  devices that wrote different roots for one successor converge. */
    private fun applyLineage(envelope: ByteArray) {
        val changed = synchronized(stateLock) {
            val state = syncedState ?: return
            try {
                state.applyLineageEnvelope(envelope)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "lineage envelope refused (${cause.javaClass.simpleName})")
                false
            }
        }
        if (changed) republish()
    }

    // ------------------------------------------------------------
    // ADR-0075 (amended 2026-10-13) - the recovery email address
    // ------------------------------------------------------------

    private val mutableRecoveryEmail: MutableStateFlow<RecoveryAddressState> =
        MutableStateFlow(RecoveryAddressState.NOT_READY)

    /**
     * The account's recovery email address as this device holds it: one
     * register of the synced state, shared with the account's other devices
     * and the sealed backup, persisted with the state under the device key.
     * Never logged.
     */
    public val recoveryEmail: StateFlow<RecoveryAddressState> = mutableRecoveryEmail.asStateFlow()

    /** Project the register. Under [stateLock]. */
    private fun republishRecoveryEmail(state: SyncedState) {
        val next = RecoveryAddressState(ready = true, address = runCatching { state.recoveryEmail() }.getOrNull())
        if (next != mutableRecoveryEmail.value) mutableRecoveryEmail.value = next
    }

    /**
     * Write the register ([address] `null` or blank = removed) and tell the
     * account's other devices. `false` when the state is not open, the
     * address is refused, or what is held is identical or newer. A failed
     * publish is not lost: a catch-up re-sends the register.
     */
    public suspend fun setRecoveryEmail(address: String?): Boolean {
        val envelope = synchronized(stateLock) {
            val state = syncedState ?: return false
            val written = try {
                state.setRecoveryEmail(Hlc.mint(), address?.takeIf { it.isNotBlank() })
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "recovery email register refused (${cause.javaClass.simpleName})")
                false
            }
            if (!written) return false
            runCatching { state.recoveryEmailEnvelope() }.getOrNull()
        }
        republish()
        if (envelope != null) {
            try {
                sendServiceEnvelope(envelope)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "recovery email register not published (${cause.javaClass.simpleName})")
                pendingFlag?.recordFailure()
            }
        }
        return true
    }

    /** A received recovery-email envelope, folded by its original stamp. */
    private fun applyRecoveryEmail(envelope: ByteArray) {
        val changed = synchronized(stateLock) {
            val state = syncedState ?: return
            try {
                state.applyRecoveryEmailEnvelope(envelope)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "recovery email envelope refused (${cause.javaClass.simpleName})")
                false
            }
        }
        if (changed) republish()
    }

    // ------------------------------------------------------------
    // ADR-0076 - the account's own username
    // ------------------------------------------------------------

    private val mutableOwnUsername: MutableStateFlow<OwnUsernameState> = MutableStateFlow(OwnUsernameState.NOT_READY)

    /** The account's username as the synced state's register holds it: what
     *  tells a device that the account was renamed on another one. */
    public val ownUsername: StateFlow<OwnUsernameState> = mutableOwnUsername.asStateFlow()

    /** Project the register. Under [stateLock]. */
    private fun republishOwnUsername(state: SyncedState) {
        val next = OwnUsernameState(ready = true, username = runCatching { state.ownUsername() }.getOrNull())
        if (next != mutableOwnUsername.value) mutableOwnUsername.value = next
    }

    /**
     * Write the register and tell the account's other devices. `false` when
     * the state is not open, the name is refused, or what is held is
     * identical or newer. A failed publish is not lost: a catch-up re-sends
     * the register.
     */
    public suspend fun setOwnUsername(username: String?): Boolean {
        val envelope = synchronized(stateLock) {
            val state = syncedState ?: return false
            val written = try {
                state.setOwnUsername(Hlc.mint(), username?.takeIf { it.isNotBlank() })
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "own username register refused (${cause.javaClass.simpleName})")
                false
            }
            if (!written) return false
            runCatching { state.ownUsernameEnvelope() }.getOrNull()
        }
        republish()
        if (envelope != null) {
            try {
                sendServiceEnvelope(envelope)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "own username register not published (${cause.javaClass.simpleName})")
                pendingFlag?.recordFailure()
            }
        }
        return true
    }

    /** A received own-username envelope, folded by its original stamp. */
    private fun applyOwnUsername(envelope: ByteArray) {
        val changed = synchronized(stateLock) {
            val state = syncedState ?: return
            try {
                state.applyOwnUsernameEnvelope(envelope)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "own username envelope refused (${cause.javaClass.simpleName})")
                false
            }
        }
        if (changed) republish()
    }

    /** Fold this device's own write in, under its own leaf. */
    private fun foldLocal(body: DeviceGroupBody, hlc: ULong) {
        val ownLeaf = runCatching { group.get()?.ownLeafIndex() }.getOrNull() ?: 0u
        fold(ParsedDeviceGroupPayload(hlc = hlc, senderLeaf = ownLeaf, body = body))
    }

    /**
     * Merge a backup's synced-state bytes into this device's state (ADR-0058
     * section 5.2): a restore with no other device alive seeds the fold from
     * it; later writes supersede by stamp as usual. The backup's state is fed
     * in as its own catch-up snapshot, so the merge is the same fold as any
     * received payload and nothing held here is lost.
     */
    public fun mergeRestoredState(bytes: ByteArray) {
        if (bytes.isEmpty()) return
        val restored = runCatching { SyncedState.fromBytes(bytes) }
            .onFailure { Log.w(LOG_TAG, "backup synced state refused", it) }
            .getOrNull() ?: return
        val parts = runCatching { restored.snapshotParts() }.getOrNull() ?: return
        for (part in parts) {
            fold(
                ParsedDeviceGroupPayload(
                    hlc = 0u,
                    senderLeaf = 0u,
                    body = DeviceGroupBody.StateSnapshot(part),
                ),
            )
        }
        // ADR-0073 R.13a - the backup payload carries the lineage registers
        // (`to_bytes`), and `snapshotParts()` does not.
        for (envelope in runCatching { restored.lineageEnvelopes() }.getOrNull().orEmpty()) applyLineage(envelope)
        // ADR-0075 - and the recovery email register, with its own stamp.
        runCatching { restored.recoveryEmailEnvelope() }.getOrNull()?.let { applyRecoveryEmail(it) }
        // ADR-0076 - and the own-username register.
        runCatching { restored.ownUsernameEnvelope() }.getOrNull()?.let { applyOwnUsername(it) }
    }

    /** Permanently destroy the repository — cancels the
     *  per-instance coroutine scope. After this, [bootstrap]
     *  no longer fires the poll loop. */
    public fun close() {
        clear()
        scope.cancel()
    }

    /** The read cursors as stored: one per conversation id, the value
     *  carrying its segment. Under [stateLock]. */
    private var conversationReadCursors: Map<String, ReadCursorState> = emptyMap()

    /** ADR-0073 R.5 - the conversation id a person-facing write for
     *  [groupId] is filed under, and the index of that group among the
     *  conversation's segments. */
    private fun conversationOf(groupId: ByteArray): Pair<ByteArray, Int> {
        val entries = mutableLineage.value
        if (entries.isEmpty()) return groupId to 0
        val lineage = me.lakko.app.chat.buildLineage(entries)
        val hex = groupIdHexOf(groupId).lowercase()
        val root = lineage.conversationOf(hex)
        if (root == hex) return groupId to 0
        val rootBytes = try {
            ByteArray(root.length / 2) { i -> root.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
        } catch (_: Throwable) {
            return groupId to 0
        }
        return rootBytes to lineage.segmentIndexOf(hex)
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
        writeLocal(
            DeviceGroupBody.Draft(
                groupId = conversationOf(groupId).first,
                text = text,
                replyToId = replyToId,
            ),
        )
    }

    /** Compose + publish a `Flags` payload. */
    public suspend fun setFlags(
        groupId: ByteArray,
        pinned: Boolean,
        muted: Boolean,
        archived: Boolean,
        unreadOverride: Boolean,
    ) {
        writeLocal(
            DeviceGroupBody.Flags(
                groupId = conversationOf(groupId).first,
                pinned = pinned,
                muted = muted,
                archived = archived,
                unreadOverride = unreadOverride,
            ),
        )
    }

    /** Compose + publish a `ReadCursor` payload. */
    public suspend fun setReadCursor(
        groupId: ByteArray,
        upToMessageId: ByteArray,
        readAtMs: ULong,
    ) {
        // ADR-0073 R.5 - one position per conversation: `message_id(8 BE)`
        // in the first segment, `message_id(8 BE) || segment(4 BE)` later.
        val (conversationId, segment) = conversationOf(groupId)
        val held = synchronized(stateLock) { conversationReadCursors[groupIdHexOf(conversationId)]?.upToMessageId }
        val bytes = if (segment == 0 && held?.let { it.size >= 12 } != true) {
            upToMessageId
        } else {
            me.lakko.app.chat.readCursorBytesForWrite(upToMessageId, segment, held) ?: return
        }
        writeLocal(
            DeviceGroupBody.ReadCursor(
                groupId = conversationId,
                upToMessageId = bytes,
                readAtMs = readAtMs,
            ),
        )
    }

    /** Compose + publish an `Order` payload. */
    public suspend fun setOrder(groupIds: List<ByteArray>) {
        writeLocal(DeviceGroupBody.Order(groupIds = groupIds))
    }

    /**
     * ADR-0058 — one folder edit: ONLY the registers that changed, each
     * already stamped by the caller. Applied here at once and then sent; an
     * edit that cannot be sent stays applied and travels with the next
     * catch-up publish (UX spec section 6).
     *
     * @throws Exception when the change set breaks a bound (the fold refuses
     *   it whole and nothing is sent).
     */
    public suspend fun applyFolderChanges(changes: FolderChangeSet) {
        val changed = synchronized(stateLock) {
            val state = syncedState ?: error("DeviceGroupSync not bootstrapped")
            state.applyFolderChanges(changes)
        }
        if (changed) republish()
        publishOrMarkPending(DeviceGroupBody.FolderChanges(changes), Hlc.mint())
    }

    /**
     * A local write: fold it in first, so it is on screen at once whatever the
     * network does, then publish. A device never receives its own DeviceGroup
     * messages back, so this fold is the only way its own write reaches its
     * own state. A publish that fails leaves the write applied here and raises
     * [pendingSync]; the next catch-up publish carries it.
     */
    private suspend fun writeLocal(body: DeviceGroupBody) {
        val hlc = Hlc.mint()
        foldLocal(body, hlc)
        publishOrMarkPending(body, hlc)
    }

    private suspend fun publishOrMarkPending(body: DeviceGroupBody, hlc: ULong) {
        // A removed device sends nothing and records nothing as waiting.
        if (mutableDeviceRemoved.value) return
        try {
            sendBody(body, hlc)
        } catch (cancellation: CancellationException) {
            pendingFlag?.recordFailure()
            throw cancellation
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "publish failed; kept locally (${cause.javaClass.simpleName})")
            pendingFlag?.recordFailure()
        }
    }

    private suspend fun sendBody(body: DeviceGroupBody, hlc: ULong) {
        check(!mutableDeviceRemoved.value) { "this device was removed from the account" }
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

    /**
     * ADR-0058 section 5.1 - publish this device's whole folded state, one
     * DeviceGroup message per part, every entry with its ORIGINAL stamp and a
     * fresh envelope stamp. Throws on the first part that does not go out.
     */
    private suspend fun publishSnapshot() {
        val parts = synchronized(stateLock) {
            val state = syncedState ?: return
            state.snapshotParts()
        }
        for (part in parts) {
            sendBody(DeviceGroupBody.StateSnapshot(part), Hlc.mint())
        }
        // ADR-0073 R.13a - the lineage registers are not in `snapshot()`: a
        // catch-up is the snapshot parts plus `lineageEnvelopes()`.
        val lineageEnvelopes = synchronized(stateLock) {
            runCatching { syncedState?.lineageEnvelopes() }.getOrNull().orEmpty()
        }
        for (envelope in lineageEnvelopes) sendServiceEnvelope(envelope)
        // ADR-0075 - the recovery email register is not in `snapshot()` either.
        val recoveryEmailEnvelope = synchronized(stateLock) {
            runCatching { syncedState?.recoveryEmailEnvelope() }.getOrNull()
        }
        recoveryEmailEnvelope?.let { sendServiceEnvelope(it) }
        // ADR-0076 - nor is the own-username register.
        val ownUsernameEnvelope = synchronized(stateLock) {
            runCatching { syncedState?.ownUsernameEnvelope() }.getOrNull()
        }
        ownUsernameEnvelope?.let { sendServiceEnvelope(it) }
    }

    /** Re-send everything this device holds if a write of ours is still
     *  waiting. Called when the network is known to work again. */
    private suspend fun flushPendingIfAny() {
        if (mutableDeviceRemoved.value) return
        val flag = pendingFlag ?: return
        if (!flag.pending) return
        val generation = flag.generation
        try {
            publishSnapshot()
            flag.onFlushed(generation)
            Log.i(LOG_TAG, "pending device-sync writes published")
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "pending flush failed (${cause.javaClass.simpleName})")
        }
    }

    /**
     * The DeviceGroup grew (ADR-0058 section 5.1): the lowest leaf among the
     * members BEFORE the growth re-sends the whole state after a random wait
     * of up to 30 seconds, so the new device starts with the pins, folders
     * and drafts it could not decrypt from the journal.
     */
    private fun onGrowth(priorLeaves: List<UInt>) {
        if (mutableDeviceRemoved.value) return
        val g = group.get() ?: return
        val ownLeaf = runCatching { g.ownLeafIndex() }.getOrNull() ?: return
        val send = runCatching { shouldSendSnapshot(ownLeaf, priorLeaves) }.getOrDefault(false)
        if (!send) return
        scope.launchBound(sessionBinding) {
            delay(SyncCatchUp.resendDelayMs(kotlin.random.Random.nextDouble()))
            try {
                publishSnapshot()
                Log.i(LOG_TAG, "growth re-send published")
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "growth re-send failed (${cause.javaClass.simpleName})")
                // The pending flush publishes the same snapshot once the
                // network is back.
                pendingFlag?.recordFailure()
            }
        }
    }

    /**
     * This device just added a device to the DeviceGroup on a handle of its
     * own (pairing, `PairingOrchestrator.runScanFlow`). A device never
     * receives its own commit back, so the poll would neither see the growth
     * nor advance this repository's handle past it: reopen the handle from
     * the snapshot the pairing persisted, then run the growth re-send with
     * the members as they were before the add.
     */
    public fun onLocalDeviceAdded(priorLeaves: List<UInt>) {
        if (!reopenAfterLocalChange(letIn = true)) return
        onGrowth(priorLeaves)
    }

    /**
     * This device changed the DeviceGroup on a handle of its own without
     * adding anyone (`DeviceRemoval`: a removal, the attested-removal policy).
     * Reopen the handle from the snapshot that change persisted: left alone,
     * it stays an epoch behind for the rest of the session and every later
     * row is refused as a wrong epoch.
     */
    public fun onLocalMembershipChanged() {
        reopenAfterLocalChange()
    }

    private fun reopenAfterLocalChange(letIn: Boolean = false): Boolean {
        if (mutableDeviceRemoved.value) return false
        val accountIdB64 = activeAccountIdB64.get() ?: return false
        val accountId = activeAccountId.get() ?: return false
        pollJob?.cancel()
        pollJob = null
        openAndPoll(accountId, accountIdB64, seedState = false, letIn = letIn)
        return true
    }

    // ------------------------------------------------------------
    // Poll loop
    // ------------------------------------------------------------

    /**
     * Returns the journal head it stopped at when the DeviceGroup forked
     * ([DeviceGroupFork]) and this session may still re-found it, else
     * `null` (cancelled, or the handle is gone).
     */
    private suspend fun runPollLoop(
        bound: me.lakko.app.Binding,
        accountIdB64: String,
        groupId: ByteArray,
        startCursor: Long = 0L,
    ): Long? {
        var cursor = startCursor
        var suspectSince: Long? = null
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

                // The fetch answered, so the network works: send whatever of
                // ours is still waiting.
                flushPendingIfAny()
                val drainedBatch = DeviceGroupFork.isDrained(result.messages.size, result.hasMore)

                // G-13 (ADR-0015): the 32-byte verifying key peers feed to
                // processIncomingVerifyingRemovals. Without it the batch is
                // HELD, not applied under a zero key: a zero key refuses a
                // removal the policy requires to be attested, and a refused
                // row is skipped for good once the cursor moves past it. A
                // device removed that way would stay an epoch behind, drain the
                // journal, read as forked and re-found the DeviceGroup over
                // every healthy device (ADR-0057, 2026-09-30).
                val attestVk: ByteArray = try {
                    DeviceAttestKey.fetch()
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (cause: Throwable) {
                    Log.w(LOG_TAG, "device-attest key fetch failed; holding the batch", cause)
                    delay(5000)
                    continue
                }

                var mutated = false
                for (message in result.messages) {
                    // The account this loop polls for is gone: the handles
                    // read below are nobody's, or the next account's.
                    if (!bound.isCurrent) return null
                    cursor = message.id
                    val g = group.get() ?: return null
                    val id = identity.get() ?: return null
                    // A commit earlier in this batch removed this device: the
                    // group is frozen, nothing after it decrypts. Stop reading
                    // the batch; the check after the persist below stops the loop.
                    if (RemovedDevice.isRemoved(RemovedDevice.readActive { g.isActive() })) break
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
                            val body = parsed.body
                            if (body is DeviceGroupBody.ServiceEnvelope &&
                                uniffi.lakko_ffi_uniffi.isOwnUsernameEnvelope(body.envelope)
                            ) {
                                // ADR-0076 - the account's username from
                                // another of its devices (a rename there).
                                applyOwnUsername(body.envelope)
                            } else if (body is DeviceGroupBody.ServiceEnvelope &&
                                uniffi.lakko_ffi_uniffi.isRecoveryEmailEnvelope(body.envelope)
                            ) {
                                // ADR-0075 - the account's recovery email
                                // address from another of its devices; a
                                // `lakko-sync` envelope, routed before the
                                // lakko-contacts dispatcher.
                                applyRecoveryEmail(body.envelope)
                            } else if (body is DeviceGroupBody.ServiceEnvelope &&
                                uniffi.lakko_ffi_uniffi.isLineageEnvelope(body.envelope)
                            ) {
                                // ADR-0073 R.13a - a lineage register from
                                // another of this account's devices. Routed
                                // BEFORE the lakko-contacts dispatcher: it is
                                // a `lakko-sync` envelope, not one of those.
                                applyLineage(body.envelope)
                            } else if (body is DeviceGroupBody.ServiceEnvelope && isKeyRotation(body.envelope)) {
                                // ADR-0074 section 3.3 - the account's new
                                // keys from the device that changed them.
                                // Meaningful ONLY here, in the account's own
                                // DeviceGroup; verified and stored as
                                // pending by the auth side, never rendered.
                                keyRotationSink?.invoke(body.envelope)
                            } else if (body is DeviceGroupBody.ServiceEnvelope) {
                                // ADR-0060 section 7 — the Visibility setting and
                                // the ReachState counters from another of this
                                // account's devices. The reach manager dispatches
                                // the envelope, keeps it only when strictly newer,
                                // and drops any other kind. Not a synced register.
                                me.lakko.app.reach.ReachManager.get(applicationContext)
                                    .adoptDeviceSync(body.envelope)
                            } else {
                                // Every other kind, the two ADR-0058 ones
                                // included, folds through the one Rust fold.
                                fold(parsed)
                            }
                        } catch (cause: Throwable) {
                            // A refused FolderChanges / StateSnapshot is
                            // skipped; the next write of those registers from
                            // any device supersedes it (ADR-0058 section 4).
                            Log.w(LOG_TAG, "parseDevicegroupPayload failed", cause)
                        }
                    } else if (processed is ProcessedMessage.Commit) {
                        val after = runCatching { g.devices().map { it.leafIndex }.toSet() }.getOrNull()
                        if (after != null) {
                            SyncCatchUp.priorLeavesIfGrown(knownLeaves, after)?.let(::onGrowth)
                            knownLeaves = after
                        }
                        // UX 17.5 - another device changed the account's
                        // device list: a device this one has not
                        // acknowledged raises the notice.
                        reportRoster(g, accountIdB64, localChange = false)
                    }
                    // Proposal / ExternalJoinProposal — no application-state
                    // mutation.
                }

                if (!bound.isCurrent) return null
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

                // ADR-0074 section 3.3 - every row up to the journal's head has
                // been applied: a superseded device waiting for a key-rotation
                // envelope may decide now.
                if (drainedBatch) headReads.value = headReads.value + 1

                // A row in this batch removed THIS device (docs/ux/second-device.md
                // section 3.1). The batch that carried the removal was persisted
                // above, so the next launch finds the inactive snapshot and shows
                // the same screen; nothing after it is polled or persisted.
                val held = group.get() ?: return null
                val step = RemovedDevice.pollStepAfterBatch(RemovedDevice.readActive { held.isActive() })
                if (step == PollStep.StopRemoved) {
                    markRemoved()
                    return null
                }

                // A drained journal whose chain is ahead of this device means
                // its DeviceGroup forked. A handle that derives no signer (a
                // removed device's inactive group) is never suspected.
                val localEpoch = currentLocalEpoch()
                val verdict = if (localEpoch == null) {
                    ForkVerdict.InSync
                } else {
                    DeviceGroupFork.forkVerdict(
                        localEpoch = localEpoch,
                        drained = DeviceGroupFork.isDrained(result.messages.size, result.hasMore),
                        latestEpoch = result.latestEpoch,
                        suspectSince = suspectSince,
                        nowMs = System.currentTimeMillis(),
                    )
                }
                when (verdict) {
                    ForkVerdict.InSync -> suspectSince = null
                    is ForkVerdict.Suspect -> suspectSince = verdict.since
                    ForkVerdict.Forked -> {
                        Log.w(
                            LOG_TAG,
                            "DeviceGroup forked: local epoch $localEpoch, DS epoch ${result.latestEpoch}",
                        )
                        // The repair re-founds the account's DeviceGroup; it never
                        // runs on a removed device. An inactive group cannot
                        // reach this point anyway (it stopped above, and its
                        // epoch comes from a DS signer an inactive group cannot
                        // derive, so `localEpoch` is null and the verdict
                        // in-sync); the guard keeps it so if either changes.
                        val repairAllowed = RemovedDevice.forkRepairAllowed(
                            group.get()?.let { h -> RemovedDevice.readActive { h.isActive() } },
                        )
                        if (repairAllowed && forkRepairedFor != accountIdB64 && rootSigner != null) {
                            forkRepairedFor = accountIdB64
                            return cursor
                        }
                        // Already re-founded this session, or no account root
                        // on this device to claim a new founding with: keep
                        // polling and do not repair again.
                        Log.w(LOG_TAG, "DeviceGroup fork not repaired in this session")
                        suspectSince = null
                    }
                }
            }
        }
        return null
    }

    /** The epoch of the DeviceGroup handle held now, read from its journal
     *  signer (the FFI `DeviceGroup` has no epoch read), or `null`. */
    private fun currentLocalEpoch(): ULong? {
        val g = group.get() ?: return null
        val id = identity.get() ?: return null
        return DeviceGroupJournalAccess.signerOf(g, id)?.epoch
    }

    /**
     * ADR-0060 section 7 — send one `lakko-contacts` envelope (Visibility or
     * ReachState, stamped by the reach manager and never re-stamped here) to
     * the account's other devices as a DeviceGroup `ServiceEnvelope` body.
     * The snapshot is persisted before the publish, so a crash in between
     * never lets a later handle reuse this message's ratchet position.
     */
    private suspend fun sendServiceEnvelope(envelope: ByteArray) {
        check(!mutableDeviceRemoved.value) { "this device was removed from the account" }
        val g = group.get() ?: error("DeviceGroupSync not bootstrapped")
        val id = identity.get() ?: error("DeviceGroupSync not bootstrapped")
        val accountIdB64 = activeAccountIdB64.get()
            ?: error("DeviceGroupSync not bootstrapped")
        val wireBytes = g.createServiceEnvelopeMessage(id, envelope, Hlc.mint())
        DeviceGroupLifecycle.persistSnapshot(
            context = applicationContext,
            identity = id,
            accountIdB64 = accountIdB64,
        )
        DsClient().use { ds ->
            ds.publishGroup(g.groupId(), DeviceGroupJournalEnvelope.encode(wireBytes, null))
        }
    }

    // ------------------------------------------------------------
    // ADR-0074 part 3 - the key-rotation envelope
    // ------------------------------------------------------------

    /** Bumped each time a poll read the journal to its head. */
    private val headReads: MutableStateFlow<Long> = MutableStateFlow(0L)

    private fun isKeyRotation(envelope: ByteArray): Boolean =
        try {
            uniffi.lakko_ffi_uniffi.dispatchIncomingEnvelope(envelope).kind ==
                uniffi.lakko_ffi_uniffi.FfiIncomingEnvelopeKind.KEY_ROTATION
        } catch (_: Throwable) {
            false
        }

    /** Wait until a poll that started after this call read the journal to
     *  its head, or [timeoutMs] passed. */
    private suspend fun awaitHead(timeoutMs: Long): Boolean {
        val seen = headReads.value
        return kotlinx.coroutines.withTimeoutOrNull(timeoutMs) {
            // Two head reads: the first may belong to a fetch that was
            // already waiting when this was asked.
            headReads.first { it >= seen + 2 }
            true
        } ?: (headReads.value > seen)
    }

    /**
     * Where a device stands in the account's DeviceGroup, for the rotation's
     * "the removed device is out first" rule: whether the LIVE roster still
     * holds [deviceKey], this handle's epoch, and the journal's newest epoch
     * as the DS states it on a signed read.
     */
    public suspend fun removalStanding(deviceKey: ByteArray): me.lakko.app.devices.RemovalStanding {
        val held = group.get()
        val inRoster = if (held == null || RemovedDevice.readActive { held.isActive() } != true) {
            null
        } else {
            try {
                held.devices().any { it.signaturePublicKey.contentEquals(deviceKey) }
            } catch (_: Throwable) {
                null
            }
        }
        val localEpoch = currentLocalEpoch()
        val journalEpoch = if (held == null) {
            null
        } else {
            try {
                DsClient().use { ds ->
                    ds.fetchGroup(groupId = held.groupId(), after = Long.MAX_VALUE - 1, limit = 1, waitSecs = 0).latestEpoch
                }
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                null
            }
        }
        return me.lakko.app.devices.RemovalStanding(inRoster, localEpoch, journalEpoch)
    }

    /** This handle's DeviceGroup epoch when [deviceKey] is in its roster. */
    public fun epochIfInRoster(deviceKey: ByteArray): ULong? {
        val held = group.get() ?: return null
        val inRoster = try {
            held.devices().any { it.signaturePublicKey.contentEquals(deviceKey) }
        } catch (_: Throwable) {
            false
        }
        return if (inRoster) currentLocalEpoch() else null
    }

    /**
     * Seal a `key-rotation` envelope as an application message of the
     * DeviceGroup at its CURRENT epoch, persist the snapshot (the ratchet
     * position is spent whether or not the publish follows), and return the
     * wire with the epoch its framing carries. Nothing is published here:
     * [me.lakko.app.devices.removeThenAnnounce] decides that.
     */
    public fun sealKeyRotation(envelope: ByteArray): Pair<ByteArray, ULong> {
        check(!mutableDeviceRemoved.value) { "this device was removed from the account" }
        val g = group.get() ?: error("DeviceGroupSync not bootstrapped")
        val id = identity.get() ?: error("DeviceGroupSync not bootstrapped")
        val accountIdB64 = activeAccountIdB64.get() ?: error("DeviceGroupSync not bootstrapped")
        val wire = g.createServiceEnvelopeMessage(id, envelope, Hlc.mint())
        DeviceGroupLifecycle.persistSnapshot(context = applicationContext, identity = id, accountIdB64 = accountIdB64)
        val sealedAt = uniffi.lakko_ffi_uniffi.epochOfMessage(wire)
            ?: error("the sealed message carries no epoch")
        return wire to sealedAt
    }

    /** Publish a message [sealKeyRotation] made into THIS account's
     *  DeviceGroup journal - the only destination it ever has. */
    public suspend fun publishKeyRotation(wire: ByteArray) {
        val g = group.get() ?: error("DeviceGroupSync not bootstrapped")
        DsClient().use { ds ->
            ds.publishGroup(g.groupId(), DeviceGroupJournalEnvelope.encode(wire, null))
        }
    }

    /** ADR-0074 section 3.5 - the account seed changed (a rotation this
     *  device ran or adopted): the journal's root signer follows it. */
    public fun rebindRoot(accountIdentitySeed: ByteArray) {
        val g = group.get() ?: return
        rootSigner = DeviceGroupJournalAccess.rootOfSeed(accountIdentitySeed, g.groupId()) ?: rootSigner
    }

    /** Route the reach manager's outgoing sync envelopes through this
     *  runtime's DeviceGroup handle. */
    private fun installReachPublisher() {
        val publisher: (ByteArray) -> Unit = { envelope ->
            val copy = envelope.copyOf()
            scope.launchBound(sessionBinding) {
                try {
                    sendServiceEnvelope(copy)
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (cause: Throwable) {
                    Log.w(LOG_TAG, "reach sync envelope not sent: ${cause.javaClass.simpleName}")
                }
            }
        }
        reachPublisher = publisher
        me.lakko.app.reach.ReachManager.get(applicationContext).deviceSyncPublisher = publisher
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
                    // ADR-0073 section 5 — after a reset of the account keys
                    // the chain on the DS belongs to the old root; the
                    // attestation is what lets this root reset it.
                    keyResetEvidence = if (unconfirmed && accountIdB64 != null) {
                        { rootKey ->
                            me.lakko.app.devices.DeviceGroupJournalAccess
                                .pendingResetEvidence(applicationContext, accountIdB64, groupId, rootKey)
                        }
                    } else {
                        null
                    },
                )
                Log.i(LOG_TAG, "DeviceGroup journal claim: $outcome")
                val landed = outcome != me.lakko.app.ds.JournalClaimOutcome.ALREADY_CLAIMED
                if (landed && accountIdB64 != null) {
                    me.lakko.app.devices.DeviceGroupJournalAccess.clearFoundingUnconfirmed(applicationContext, accountIdB64)
                    me.lakko.app.devices.DeviceGroupJournalAccess.onJournalRootLanded(applicationContext, accountIdB64)
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
        // First: the poll loop and everything launched for this account stop
        // before the handles below are replaced.
        sessionBinding.advance()
        // Write a change still waiting out its debounce, then let the account
        // go: the state is kept for the next sign-in, never written after.
        syncStatePersistence.stopAndFlush()
        // The same for the pending flag: a publish still in flight may fail
        // after this, and must not write the row back (Forget device deletes
        // it right after this teardown).
        pendingFlag?.detach()
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
        reachPublisher?.let { installed ->
            me.lakko.app.reach.ReachManager.peek()?.let { reach ->
                if (reach.deviceSyncPublisher === installed) reach.deviceSyncPublisher = null
            }
        }
        reachPublisher = null
        DsSignerRegistry.shared.clearOwner(DeviceGroupJournalAccess.REGISTRY_OWNER)
    }

    /** Test seam: bind [accountIdB64] as the running account without opening
     *  a DeviceGroup (which needs the native core and the Keystore), so a
     *  JVM test can drive [clear] against real on-disk rows. */
    internal fun bindAccountForTest(accountIdB64: String) {
        activeAccountIdB64.set(accountIdB64)
        syncStatePersistence.start(accountIdB64)
    }

    public companion object {
        private const val LOG_TAG = "DeviceGroupSync"

        /** Per-account "a write is waiting" flag. Not secret: a boolean. */
        internal const val PENDING_PREFS = "lakko-device-sync-pending"

        /**
         * Forget device: delete [accountIdB64]'s persisted folded state (the
         * synced drafts, flags, read cursors, order and folders) and its
         * "a write is waiting" flag. Other accounts' rows stay. The one caller
         * is [me.lakko.app.account.AccountLocalWipe].
         */
        public fun deleteAccountRows(context: Context, accountIdB64: String) {
            DeviceGroupSnapshotStore.deleteSnapshot(
                context = context,
                accountIdB64 = accountIdB64,
                dir = DeviceGroupSnapshotStore.SYNC_STATE_DIR,
            )
            context.applicationContext.getSharedPreferences(PENDING_PREFS, Context.MODE_PRIVATE)
                .edit()
                .remove(accountIdB64)
                .apply()
        }

        /**
         * The repository of the signed-in session, for the one caller that
         * cannot be handed it: pairing, which adds a device on its own
         * DeviceGroup handle and must tell this one (see [onLocalDeviceAdded]).
         */
        @Volatile
        public var active: DeviceGroupSyncRepository? = null
            private set

        /** ADR-0074 section 3.3 - where a `key-rotation` envelope read from
         *  the account's own DeviceGroup goes (the auth side verifies it and
         *  stores it as pending). Set by the app root. */
        @Volatile
        public var keyRotationSink: ((ByteArray) -> Unit)? = null

        /**
         * Wait until the live runtime read its journal to the head, once.
         * The runtime may not be up yet right after a launch: waits for it
         * within the same [timeoutMs]. `false` when the head was not seen.
         */
        public suspend fun awaitHeadOnce(timeoutMs: Long): Boolean {
            val deadline = System.currentTimeMillis() + timeoutMs
            while (System.currentTimeMillis() < deadline) {
                val live = active?.takeIf { it.group.get() != null && it.pollJob?.isActive == true }
                if (live != null) return live.awaitHead((deadline - System.currentTimeMillis()).coerceAtLeast(0))
                delay(AWAIT_RUNTIME_STEP_MS)
            }
            return false
        }

        private const val AWAIT_RUNTIME_STEP_MS: Long = 500

        /**
         * The folded state's bytes for [accountIdB64] as a backup carries
         * them (ADR-0058 section 5.2), for a caller that runs before - or
         * without - the sync runtime: the live repository's when it is on
         * this account, else what the state file holds. `null` when there is
         * none, or it cannot be read.
         */
        public fun backupSyncedStateBytes(context: Context, accountIdB64: String): ByteArray? {
            val live = active
            if (live != null && live.activeAccountIdB64.get() == accountIdB64) {
                return live.currentSyncedStateBytes()
            }
            return try {
                // A file this build's core refuses (an older encoding
                // version) is "none held": the backup carries no synced
                // state rather than bytes no device could merge back.
                heldSyncedStateBytes(
                    stored = DeviceGroupSnapshotStore.getSnapshot(
                        context = context,
                        accountIdB64 = accountIdB64,
                        dir = DeviceGroupSnapshotStore.SYNC_STATE_DIR,
                    ),
                    core = CoreSyncedStateBytes,
                ) { cause ->
                    Log.w(
                        LOG_TAG,
                        "persisted synced state refused, the backup carries none (${cause.javaClass.simpleName})",
                    )
                }
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "synced state not readable for the backup (${cause.javaClass.simpleName})")
                null
            }
        }

        /**
         * A backup restore carried folded state (ADR-0058 section 5.2). The
         * restore runs before the sync runtime bootstraps, so the state goes
         * to the same file [bootstrap] seeds from, merged with anything this
         * device already held; a live repository for the account folds it in
         * too. Best-effort: a failure leaves the device to catch up from its
         * other devices.
         */
        public fun seedFromBackup(context: Context, accountIdB64: String, bytes: ByteArray) {
            if (bytes.isEmpty()) return
            val live = active
            if (live != null && live.activeAccountIdB64.get() == accountIdB64) {
                live.mergeRestoredState(bytes)
                return
            }
            try {
                // Bytes the core refuses (an older encoding version) are
                // "none held", in the backup and in the file alike: a
                // refused backup seeds nothing, a refused file is replaced.
                val merged = backupSeedBytes(
                    stored = DeviceGroupSnapshotStore.getSnapshot(
                        context = context,
                        accountIdB64 = accountIdB64,
                        dir = DeviceGroupSnapshotStore.SYNC_STATE_DIR,
                    ),
                    backup = bytes,
                    core = CoreSyncedStateBytes,
                ) { which, cause ->
                    Log.w(
                        LOG_TAG,
                        "backup seed: synced state refused (${which.name}, ${cause.javaClass.simpleName})",
                    )
                } ?: return
                DeviceGroupSnapshotStore.putSnapshot(
                    context = context,
                    accountIdB64 = accountIdB64,
                    snapshotBytes = merged,
                    dir = DeviceGroupSnapshotStore.SYNC_STATE_DIR,
                )
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "backup synced state not seeded (${cause.javaClass.simpleName})")
            }
        }
    }
}

/** [SyncedStateBytes] over the native core. */
internal object CoreSyncedStateBytes : SyncedStateBytes {
    override fun canonical(bytes: ByteArray): ByteArray =
        SyncedState.fromBytes(bytes).use { it.toBytes() }

    override fun merged(existing: ByteArray, backup: ByteArray): ByteArray =
        SyncedState.fromBytes(existing).use { state ->
            SyncedState.fromBytes(backup).use { restored ->
                for (part in restored.snapshotParts()) {
                    state.applyPayload(
                        ParsedDeviceGroupPayload(
                            hlc = 0u,
                            senderLeaf = 0u,
                            body = DeviceGroupBody.StateSnapshot(part),
                        ),
                    )
                }
                // ADR-0073 R.13a - and the lineage registers, which are not
                // part of `snapshotParts()`.
                for (envelope in restored.lineageEnvelopes()) {
                    state.applyLineageEnvelope(envelope)
                }
                // ADR-0075 - and the recovery email register.
                restored.recoveryEmailEnvelope()?.let { state.applyRecoveryEmailEnvelope(it) }
                // ADR-0076 - and the own-username register.
                restored.ownUsernameEnvelope()?.let { state.applyOwnUsernameEnvelope(it) }
            }
            state.toBytes()
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
