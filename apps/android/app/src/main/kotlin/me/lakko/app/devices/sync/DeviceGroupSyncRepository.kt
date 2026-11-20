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
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.async
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.selects.select
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
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
import uniffi.lakko_ffi_uniffi.FolderConversationKind
import uniffi.lakko_ffi_uniffi.FolderConversationRef
import uniffi.lakko_ffi_uniffi.FolderView
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.LinkAnswerVerdict
import uniffi.lakko_ffi_uniffi.LinkRecord
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
    private val mutableSaved: MutableStateFlow<List<uniffi.lakko_ffi_uniffi.SavedMessageView>> =
        MutableStateFlow(emptyList())

    /** ADR-0065 section 4 — the account's saved messages, as the fold holds
     *  them (every device of the account shows the same list). */
    public val saved: StateFlow<List<uniffi.lakko_ffi_uniffi.SavedMessageView>> = mutableSaved.asStateFlow()

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

    /** UX second-device.md section 3 - an open Devices screen hears of a
     *  received commit once its snapshot is on disk. */
    private val rosterChange = me.lakko.app.devices.RosterChangeAfterPersist {
        me.lakko.app.devices.DeviceRosterChanges.note()
    }

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

    /** ADR-0081 decision 3 — the snapshot generation the held handle was read
     *  at (or last wrote). Every write of the handle names it, so a handle
     *  still at N never puts N back over an N+1 another handle made current. */
    @Volatile private var handleGeneration: Int? = null

    /** ADR-0081 decision 4 — the founding decision or rejoin in progress. */
    @Volatile private var rejoinJob: Job? = null

    /** ADR-0081 decision 4 — the sibling's admission record. */
    @Volatile private var admissions: AdmissionRecord? = null

    /** The DS id of the last knock row the admission check handled this
     *  session. */
    @Volatile private var knockCursor: Long = 0L

    /** ADR-0081 third amendment — the knock listener read a knock the
     *  admission check has not handled yet; [knockFetchNow] makes the poll's
     *  next read immediate so the check runs at once. */
    private val knocksPending = java.util.concurrent.atomic.AtomicBoolean(false)
    private val knockFetchNow = java.util.concurrent.atomic.AtomicBoolean(false)

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
            me.lakko.app.backup.markBackupDueForSyncedState(applicationContext, accountIdB64)
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
        } catch (_: DeviceGroupLifecycle.NotJoined) {
            // ADR-0081 decision 4 — no usable DeviceGroup state: found only
            // when the directory lists no other own device, rejoin otherwise.
            startFoundOrRejoin(accountIdBytes, accountIdB64, seed, seedState, accountIdentitySeed)
            return
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "ensureDeviceGroup failed", cause)
            activeAccountIdB64.set(null)
            seed.fill(0)
            return
        }
        seed.fill(0)
        adopt(ensured, accountIdBytes, accountIdB64, seedState, accountIdentitySeed, startCursor, letIn)
    }

    /** Take [ensured] as this session's DeviceGroup handle and start following
     *  it: the removed check, the synced state, the roster, the poll loop. */
    private fun adopt(
        ensured: DeviceGroupLifecycle.Ensured,
        accountIdBytes: ByteArray,
        accountIdB64: String,
        seedState: Boolean,
        accountIdentitySeed: ByteArray?,
        startCursor: Long,
        letIn: Boolean,
    ) {
        identity.set(ensured.identity)
        group.set(ensured.group)
        handleGeneration = ensured.generation
        loadAdmissions(accountIdB64)

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
        // cannot refill it: the poll resumes from the stored cursor (0 on a
        // first run), and the MLS snapshot was restored above, so any row it
        // re-fetches is one this state has already consumed. The snapshot is
        // a head start, not an authority - the poll folds newer rows on top,
        // so a sibling device's later write still wins. Bytes in an older
        // format (encoding version 1, before the saved-message register) do
        // not decode and are dropped: there is no migration (no production
        // data). The state
        // starts empty, and the persistence writer's first write replaces the
        // refused file.
        if (seedState) {
            val stored = runCatching {
                DeviceGroupSnapshotStore.getSnapshot(
                    context = applicationContext,
                    accountIdB64 = accountIdB64,
                    dir = DeviceGroupSnapshotStore.SYNC_STATE_DIR,
                )
            }.onFailure { Log.w(LOG_TAG, "persisted synced state not read", it) }.getOrNull()
            val seeded = seedStateOrFresh(
                stored = stored,
                open = { SyncedState.fromBytes(it) },
                fresh = { SyncedState() },
                onRefused = { Log.w(LOG_TAG, "persisted synced state dropped", it) },
            )
            synchronized(stateLock) {
                syncedState = seeded
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
        me.lakko.app.devices.NewDeviceNotices.bind(applicationContext, accountIdB64) { keys ->
            scope.launch { acknowledgeDevices(keys) }
        }
        // UX 17.6 - the account's acknowledged devices first, so a device seen
        // on another one is never announced here by the roster that follows.
        reportAcknowledged(accountIdB64)
        // ADR-0080 - this account's device cards, sent through this
        // runtime's DeviceGroup `ServiceEnvelope`.
        me.lakko.app.devices.DeviceCardCenter.bind(applicationContext, accountIdB64) { envelope ->
            sendServiceEnvelope(envelope)
        }
        reportRoster(ensured.group, accountIdB64, localChange = letIn)
        me.lakko.app.devices.DeviceRosterChanges.note()
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
        } else if (rootSigner == null) {
            // A rejoin that ended in a founding starts with no seed in hand;
            // the root of this journal comes from the stored account seed.
            rootSigner = DeviceGroupJournalAccess.rootOf(applicationContext, accountIdB64, groupId)
        }
        installJournalSigner(groupId)
        installReachPublisher()
        val bound = sessionBinding.capture()
        knocksPending.set(false)
        knockFetchNow.set(false)
        pollJob = scope.launch {
            // ADR-0081 third amendment — this device answers knocks while it
            // shows; the listener ends with the poll loop.
            val listener = launch { runKnockListener(bound, accountIdB64, groupId) }
            try {
                claimJournalBestEffort(groupId, refounded = true)
                runPollLoop(
                    bound = bound,
                    accountIdBytes = accountIdBytes,
                    accountIdB64 = accountIdB64,
                    groupId = groupId,
                    startCursor = startCursor,
                )
            } finally {
                listener.cancel()
            }
        }
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
        me.lakko.app.devices.DeviceCardCenter.unbind()
        cancelPollAndDropHandles()
        activeAccountIdB64.set(null)
        activeAccountId.set(null)
        me.lakko.app.chat.ConversationEndsMirror.publish(null)
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
        mutableChannelSubscriptions.value = null
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
                me.lakko.app.devices.NewDeviceNotices.onRoster(accountIdB64, roster, self)
            }
            // ADR-0080 - a leaf that left takes its card with it.
            me.lakko.app.devices.DeviceCardCenter.prune(rows.map { hexOf(it.signaturePublicKey) })
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "device roster not read for the new-device notice (${cause.javaClass.simpleName})")
        }
    }

    /**
     * ADR-0080 - take in [envelope] when it is a device card. `true` when it
     * was one (kept or dropped), `false` for any other envelope. The sender
     * is the MLS-authenticated leaf of the row, resolved against the roster
     * at receipt; a row with no sender leaf, or a leaf the roster does not
     * hold, is dropped.
     */
    private fun receiveDeviceCard(envelope: ByteArray, senderLeaf: UInt?, g: DeviceGroup): Boolean {
        val card = try {
            uniffi.lakko_ffi_uniffi.decodeDeviceCardEnvelope(envelope)
        } catch (_: Throwable) {
            return false
        }
        val center = me.lakko.app.devices.DeviceCardCenter
        val leaf = senderLeaf ?: return true.also { center.noteRefused("no sender") }
        val roster = runCatching { g.devices() }.getOrNull() ?: return true.also { center.noteRefused("roster not read") }
        val sender = roster.firstOrNull { it.leafIndex == leaf }
            ?: return true.also { center.noteRefused("sender not in roster") }
        me.lakko.app.devices.DeviceCardCenter.receive(
            hexOf(sender.signaturePublicKey),
            me.lakko.app.devices.DeviceCardCenter.fromFfi(card),
            roster.map { hexOf(it.signaturePublicKey) },
        )
        return true
    }

    private fun hexOf(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

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
                republishQuickReaction(state)
                republishDeviceAcks(state)
                republishLinkRecords(state)
                republishChannelSubscriptions(state)
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
                mutableSaved.value = state.saved()
                republishConversationEnds(state)
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
    // ADR-0087 section 9 - the end of a membership
    // ------------------------------------------------------------

    /** Project the ended registers for the chat layer. Under [stateLock]. */
    private fun republishConversationEnds(state: SyncedState) {
        val account = activeAccountIdB64.get() ?: return
        val ends = try {
            state.endedConversations().map {
                me.lakko.app.chat.HeardConversationEnd(
                    idHex = groupIdHexOf(it.conversation.id).lowercase(),
                    channel = it.conversation.kind == FolderConversationKind.CHANNEL,
                    epoch = it.epoch,
                )
            }
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "conversation ends not read (${cause.javaClass.simpleName})")
            return
        }
        me.lakko.app.chat.ConversationEndsMirror.publish(me.lakko.app.chat.HeardConversationEnds(account, ends))
    }

    /** Whether the fold says the account is out of [conversation]. */
    public fun isConversationEnded(conversation: FolderConversationRef): Boolean = synchronized(stateLock) {
        try {
            syncedState?.isConversationEnded(conversation) == true
        } catch (_: Throwable) {
            false
        }
    }

    /**
     * Tell the account's other devices the account is out of [conversation],
     * at the MLS [epoch] of the end (0 for a channel).
     */
    public suspend fun publishConversationEnded(
        conversation: FolderConversationRef,
        epoch: ULong,
    ): me.lakko.app.chat.EndPublish = publishEndRegister { state, hlc ->
        state.composeConversationEnded(hlc, conversation, epoch)
    }

    /**
     * Tell them the account is back in [conversation], at the epoch the join
     * brought it to. A first join finds no register and writes none.
     */
    public suspend fun publishConversationJoined(
        conversation: FolderConversationRef,
        epoch: ULong,
    ): me.lakko.app.chat.EndPublish = publishEndRegister { state, hlc ->
        state.composeConversationJoined(hlc, conversation, epoch)
    }

    private suspend fun publishEndRegister(
        compose: (SyncedState, ULong) -> uniffi.lakko_ffi_uniffi.ConversationEndRegister?,
    ): me.lakko.app.chat.EndPublish {
        val register = synchronized(stateLock) {
            val state = syncedState ?: return me.lakko.app.chat.EndPublish.NOT_OPEN
            try {
                compose(state, Hlc.mint())
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "conversation end register refused (${cause.javaClass.simpleName})")
                null
            }
        } ?: return me.lakko.app.chat.EndPublish.NOTHING_TO_TELL
        try {
            applyFolderChanges(
                FolderChangeSet(
                    metas = emptyList(),
                    members = emptyList(),
                    pins = emptyList(),
                    removed = emptyList(),
                    order = null,
                    conversationEnds = listOf(register),
                ),
            )
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "conversation end register not applied (${cause.javaClass.simpleName})")
            return me.lakko.app.chat.EndPublish.NOTHING_TO_TELL
        }
        // Past the debounce: the owed record moves on once this returns, and
        // nothing else would write this register again.
        syncStatePersistence.writeNow()
        return me.lakko.app.chat.EndPublish.PUBLISHED
    }

    /**
     * Clear what the synced state still holds of [conversation], each register
     * by its own tombstone: saved messages, folder rows and folder pins, flags,
     * the read position, the draft, the place in the order. Only what is held
     * is written, so a device that hears of an end after the others cleared it
     * sends nothing. The synced register of a profile is not tied to a group.
     */
    public suspend fun clearConversationRegisters(conversation: FolderConversationRef) {
        val groupId = conversation.id
        val conversationId = conversationOf(groupId).first
        class Held(
            val saved: List<uniffi.lakko_ffi_uniffi.SavedMessageView>,
            val folders: List<me.lakko.app.chat.ChatFolder>,
            val draft: Boolean,
            val flags: Boolean,
            val cursor: Boolean,
            val order: List<ByteArray>?,
        )
        val held = synchronized(stateLock) {
            val state = syncedState ?: return
            try {
                Held(
                    saved = state.saved().filter { hexEquals(it.conversation.id, groupId) || hexEquals(it.conversation.id, conversationId) },
                    folders = state.folders().map(me.lakko.app.chat.ChatFolder::of),
                    draft = state.draft(conversationId)?.text?.isNotEmpty() == true,
                    flags = state.flags(conversationId)?.let { it.pinned || it.muted || it.archived || it.unreadOverride } == true,
                    cursor = state.readCursor(conversationId)?.upToMessageId?.isNotEmpty() == true,
                    order = state.conversationOrder()?.groupIds?.takeIf { ids -> ids.any { hexEquals(it, conversationId) } },
                )
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "conversation registers not read (${cause.javaClass.simpleName})")
                return
            }
        }
        val savedRegisters = held.saved.map {
            uniffi.lakko_ffi_uniffi.composeSavedRemoval(Hlc.mint(), it.conversation, it.messageId)
        }
        if (savedRegisters.isNotEmpty()) {
            runCatching { applySavedChanges(savedRegisters) }
                .onFailure { if (it is CancellationException) throw it }
        }
        val folderRef = FolderConversationRef(kind = conversation.kind, id = conversationId)
        me.lakko.app.chat.conversationRemovalChanges(held.folders, folderRef) { Hlc.mint() }?.let { changes ->
            runCatching { applyFolderChanges(changes) }
                .onFailure { if (it is CancellationException) throw it }
        }
        if (held.draft) setDraft(groupId, "")
        if (held.flags) setFlags(groupId, pinned = false, muted = false, archived = false, unreadOverride = false)
        if (held.cursor) {
            writeLocal(DeviceGroupBody.ReadCursor(groupId = conversationId, upToMessageId = ByteArray(0), readAtMs = 0u))
        }
        held.order?.let { ids -> setOrder(ids.filterNot { hexEquals(it, conversationId) }) }
    }

    private fun hexEquals(a: ByteArray, b: ByteArray): Boolean = a.contentEquals(b)

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
    // reactions-and-emoji D3 - the account's quick reaction
    // ------------------------------------------------------------

    /** Project the register into [me.lakko.app.chat.QuickReaction]. Under
     *  [stateLock]. */
    private fun republishQuickReaction(state: SyncedState) {
        me.lakko.app.chat.QuickReaction.held(runCatching { state.quickReaction() }.getOrNull())
    }

    /**
     * Set the account's quick reaction and tell its other devices. `false`
     * when the state is not open or the value is refused. A failed publish is
     * re-sent by a catch-up.
     */
    public suspend fun setQuickReaction(emoji: String): Boolean {
        val envelope = synchronized(stateLock) {
            val state = syncedState ?: return false
            val written = try {
                state.setQuickReaction(Hlc.mint(), emoji)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "quick reaction refused (${cause.javaClass.simpleName})")
                false
            }
            if (!written) return false
            runCatching { state.quickReactionEnvelope() }.getOrNull()
        }
        republish()
        if (envelope != null) {
            try {
                sendServiceEnvelope(envelope)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "quick reaction not published (${cause.javaClass.simpleName})")
                pendingFlag?.recordFailure()
            }
        }
        return true
    }

    /** A received quick-reaction envelope, folded by its original stamp. */
    private fun applyQuickReaction(envelope: ByteArray) {
        val changed = synchronized(stateLock) {
            val state = syncedState ?: return
            try {
                state.applyQuickReactionEnvelope(envelope)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "quick reaction envelope refused (${cause.javaClass.simpleName})")
                false
            }
        }
        if (changed) republish()
    }

    // ------------------------------------------------------------
    // ADR-0087 section 13a - this account's own channel reactions and comments
    // ------------------------------------------------------------

    /** The emoji this account holds on a channel post, every device's. */
    public fun channelReactionsOn(channelId: ByteArray, postCursor: ULong): List<String> = synchronized(stateLock) {
        runCatching { syncedState?.channelReactionsOn(channelId, postCursor) }.getOrNull().orEmpty()
    }

    /** The emoji this account holds on a comment under a channel post,
     *  every device's (ADR-0087 phase B). */
    public fun commentReactionsOn(channelId: ByteArray, postCursor: ULong, commentCursor: ULong): List<String> =
        synchronized(stateLock) {
            runCatching { syncedState?.commentReactionsOn(channelId, postCursor, commentCursor) }.getOrNull().orEmpty()
        }

    /**
     * Record a reaction on a comment added or taken off and tell the other
     * devices. `false` when the register refused it (a fourth reaction on
     * one comment, or no synced state): nothing is to be sent then. `true`
     * when it is recorded, or nothing changed.
     */
    public suspend fun setCommentReaction(
        channelId: ByteArray,
        postCursor: ULong,
        commentCursor: ULong,
        emoji: String,
        reacted: Boolean,
        postAtMs: ULong,
    ): Boolean {
        val envelope = synchronized(stateLock) {
            val state = syncedState ?: return false
            try {
                state.setCommentReaction(Hlc.mint(), channelId, postCursor, commentCursor, emoji, reacted, postAtMs)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "comment reaction refused by the register (${cause.javaClass.simpleName})")
                return false
            }
        } ?: return true
        republish()
        try {
            sendServiceEnvelope(envelope)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "comment reaction register not published (${cause.javaClass.simpleName})")
            pendingFlag?.recordFailure()
        }
        return true
    }

    /** The nonces of this account's live comments under a channel post. */
    public fun channelCommentsOn(channelId: ByteArray, postCursor: ULong): List<ByteArray> = synchronized(stateLock) {
        runCatching { syncedState?.channelCommentsOn(channelId, postCursor) }.getOrNull().orEmpty()
    }

    /** Record a reaction added or taken off, and tell the other devices. */
    public suspend fun setChannelReaction(
        channelId: ByteArray,
        postCursor: ULong,
        emoji: String,
        reacted: Boolean,
        postAtMs: ULong,
    ) = sendRegister {
        it.setChannelReaction(Hlc.mint(), channelId, postCursor, emoji, reacted, postAtMs)
    }

    /** Record a comment written (its nonce) and tell the other devices. */
    public suspend fun recordChannelComment(channelId: ByteArray, postCursor: ULong, nonce: ByteArray, postAtMs: ULong) =
        sendRegister { it.recordChannelComment(Hlc.mint(), channelId, postCursor, nonce, postAtMs) }

    /** Record a comment deleted and tell the other devices. */
    public suspend fun deleteChannelComment(channelId: ByteArray, postCursor: ULong, nonce: ByteArray, postAtMs: ULong) =
        sendRegister { it.deleteChannelComment(Hlc.mint(), channelId, postCursor, nonce, postAtMs) }

    private suspend fun sendRegister(write: (SyncedState) -> ByteArray?) {
        val envelope = synchronized(stateLock) {
            val state = syncedState ?: return
            try {
                write(state)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "channel feedback register refused (${cause.javaClass.simpleName})")
                null
            }
        } ?: return
        republish()
        try {
            sendServiceEnvelope(envelope)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "channel feedback register not published (${cause.javaClass.simpleName})")
            pendingFlag?.recordFailure()
        }
    }

    private fun applyChannelFeedback(envelope: ByteArray, reaction: Boolean) {
        val changed = synchronized(stateLock) {
            val state = syncedState ?: return
            try {
                if (reaction) state.applyChannelReactionEnvelope(envelope) else state.applyChannelCommentEnvelope(envelope)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "channel feedback envelope refused (${cause.javaClass.simpleName})")
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

    // ------------------------------------------------------------
    // UX 17.5 / 17.6 - the account's acknowledged new devices
    // ------------------------------------------------------------

    /** Hand the account's acknowledged devices to the new-device notice.
     *  Under [stateLock]. */
    private fun republishDeviceAcks(state: SyncedState) {
        val accountIdB64 = activeAccountIdB64.get() ?: return
        val keys = runCatching { state.acknowledgedDevices() }.getOrNull() ?: return
        me.lakko.app.devices.NewDeviceNotices.onAcknowledged(
            accountIdB64,
            keys.mapTo(HashSet()) { me.lakko.app.devices.newDeviceKey(it) },
        )
    }

    /** [republishDeviceAcks] outside a republish (right after the notice is
     *  bound). */
    private fun reportAcknowledged(accountIdB64: String) {
        synchronized(stateLock) {
            val state = syncedState ?: return
            if (activeAccountIdB64.get() != accountIdB64) return
            republishDeviceAcks(state)
        }
    }

    /**
     * Settings > Devices was opened with [keysHex] pending: write the
     * account's acknowledgement for them (write-once per device) and tell
     * the other devices. A send that fails is owed: the pending flag makes
     * the next flush re-send every acknowledgement ([publishSnapshot]).
     */
    private suspend fun acknowledgeDevices(keysHex: Set<String>) {
        val keys = keysHex.mapNotNull { me.lakko.app.devices.newDeviceKeyBytes(it) }
        if (keys.isEmpty()) return
        val envelopes = synchronized(stateLock) {
            val state = syncedState ?: return
            try {
                state.acknowledgeDevices(Hlc.mint(), keys, System.currentTimeMillis().toULong())
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "device acknowledgement refused (${cause.javaClass.simpleName})")
                emptyList()
            }
        }
        if (envelopes.isEmpty()) return
        republish()
        for (envelope in envelopes) {
            try {
                sendServiceEnvelope(envelope)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "device acknowledgement not published (${cause.javaClass.simpleName})")
                pendingFlag?.recordFailure()
                return
            }
        }
    }

    /**
     * ADR-0092 section 3.6 — record this account's own choice in a poll in the
     * synced register and tell the other devices, so every device of the
     * account shows the same own vote and a re-vote draws the same anonymity
     * set ([setVersion]). A send that fails is owed like any other write.
     */
    public fun recordPollChoice(
        conversation: FolderConversationRef,
        pollMessageId: Long,
        choices: ByteArray,
        setVersion: UInt,
    ) {
        scope.launch {
            val envelope = synchronized(stateLock) {
                val state = syncedState ?: return@launch
                try {
                    state.setPollChoice(Hlc.mint(), conversation, pollMessageId, choices, setVersion)
                } catch (cause: Throwable) {
                    Log.w(LOG_TAG, "poll choice refused (${cause.javaClass.simpleName})")
                    null
                }
            } ?: return@launch
            republish()
            try {
                sendServiceEnvelope(envelope)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "poll choice not published (${cause.javaClass.simpleName})")
                pendingFlag?.recordFailure()
            }
        }
    }

    /** This account's recorded choice in a poll, from any of its devices. */
    public fun pollChoice(conversation: FolderConversationRef, pollMessageId: Long): uniffi.lakko_ffi_uniffi.PollChoice? =
        synchronized(stateLock) {
            runCatching { syncedState?.pollChoice(conversation, pollMessageId) }.getOrNull()
        }

    /** A received own poll choice from another device of the account. */
    private fun applyPollChoice(envelope: ByteArray) {
        val changed = synchronized(stateLock) {
            val state = syncedState ?: return
            try {
                state.applyPollChoiceEnvelope(envelope)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "poll choice envelope refused (${cause.javaClass.simpleName})")
                false
            }
        }
        if (changed) republish()
    }

    /** A received device acknowledgement, folded write-once. */
    private fun applyDeviceAck(envelope: ByteArray) {
        val changed = synchronized(stateLock) {
            val state = syncedState ?: return
            try {
                state.applyDeviceAckEnvelope(envelope)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "device acknowledgement envelope refused (${cause.javaClass.simpleName})")
                false
            }
        }
        if (changed) republish()
    }

    // ------------------------------------------------------------
    // ADR-0079 - the account's own invite links
    // ------------------------------------------------------------

    private val mutableLinkRecords: MutableStateFlow<List<LinkRecord>> = MutableStateFlow(emptyList())

    /** The account's own invite links, every device's, revoked ones included
     *  (ADR-0079 section 4). */
    public val linkRecords: StateFlow<List<LinkRecord>> = mutableLinkRecords.asStateFlow()

    /** Project the register. Under [stateLock]. */
    private fun republishLinkRecords(state: SyncedState) {
        val next = runCatching { state.allLinkRecords() }.getOrNull().orEmpty()
        if (next != mutableLinkRecords.value) mutableLinkRecords.value = next
    }

    // ------------------------------------------------------------
    // docs/ux/second-device.md section 4a - the account's channel
    // subscription register (lakko-sync `channel_subscription`)
    // ------------------------------------------------------------

    private val mutableChannelSubscriptions: MutableStateFlow<List<uniffi.lakko_ffi_uniffi.ChannelSubscriptionRegister>?> =
        MutableStateFlow(null)

    /** Every channel subscription register the fold holds, leaves included,
     *  ascending by channel id; `null` while the state is not open. */
    public val channelSubscriptions: StateFlow<List<uniffi.lakko_ffi_uniffi.ChannelSubscriptionRegister>?> =
        mutableChannelSubscriptions.asStateFlow()

    /** Project the register. Under [stateLock]. */
    private fun republishChannelSubscriptions(state: SyncedState) {
        val next = runCatching { state.allChannelSubscriptions() }.getOrNull() ?: return
        val held = mutableChannelSubscriptions.value
        if (held == null || held != next) mutableChannelSubscriptions.value = next
    }

    /** The account this repository is bound to (standard base64), or
     *  `null` between sessions. */
    public val boundAccountIdB64: String?
        get() = activeAccountIdB64.get()

    /**
     * Write [channelId]'s subscription register and send it to the account's
     * other devices. The fold is written at once (a reconcile that reads the
     * register next sees it); the send runs in the background, and a failed
     * send is kept by the pending flag and re-sent with the catch-up.
     *
     * [expectedHlc]: when given, the write happens only while the register
     * held for [channelId] still carries that stamp (`0` = none held), so a
     * reconcile planned against an older register never overwrites a leave
     * written meanwhile. `false` when the state is not open, the value is
     * refused, the identical value is held, or the expectation failed.
     */
    public fun setChannelSubscription(
        channelId: ByteArray,
        role: uniffi.lakko_ffi_uniffi.ChannelSubscriptionRole,
        subscriberSecret: ByteArray,
        readKey: ByteArray,
        publisherGroupId: ByteArray,
        /** ADR-0090 — 32 bytes for a co-publisher, else empty. */
        ownerAccountKey: ByteArray,
        name: String,
        handle: String,
        expectedHlc: ULong? = null,
    ): Boolean = writeChannelSubscription(channelId) { state ->
        if (expectedHlc != null) {
            val held = runCatching { state.channelSubscription(channelId) }.getOrNull()?.hlc ?: 0uL
            if (held != expectedHlc) return@writeChannelSubscription false
        }
        state.setChannelSubscription(Hlc.mint(), channelId, role, subscriberSecret, readKey, publisherGroupId, ownerAccountKey, name, handle)
    }

    /** Write the leave of [channelId] at once and send it. `false` when
     *  nothing was held or it was already left. */
    public fun leaveChannelSubscription(channelId: ByteArray): Boolean =
        writeChannelSubscription(channelId) { it.leaveChannelSubscription(Hlc.mint(), channelId) }

    private fun writeChannelSubscription(channelId: ByteArray, write: (SyncedState) -> Boolean): Boolean {
        val envelope = synchronized(stateLock) {
            val state = syncedState ?: return false
            val written = try {
                write(state)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "channel subscription refused (${cause.javaClass.simpleName})")
                false
            }
            if (!written) return false
            runCatching { state.channelSubscriptionEnvelopeFor(channelId) }.getOrNull()
        }
        republish()
        if (envelope != null) {
            scope.launchBound(sessionBinding) {
                try {
                    sendServiceEnvelope(envelope)
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (cause: Throwable) {
                    Log.w(LOG_TAG, "channel subscription not published (${cause.javaClass.simpleName})")
                    pendingFlag?.recordFailure()
                }
            }
        }
        return true
    }

    // ------------------------------------------------------------
    // ADR-0090 section 3 / open question 4 - the owner account's channel
    // admission seeds (lakko-sync `channel_admission_seed`)
    // ------------------------------------------------------------

    /**
     * Write one admission seed of [channelId] at [version] and send it to the
     * account's other devices. Write-once per seed; another seed of the same
     * version is kept beside it (two owner devices minting one version at
     * once both survive). `false` when the state is not open, the seed was
     * refused, or it was already held.
     */
    public fun setChannelAdmissionSeed(channelId: ByteArray, version: UShort, seed: ByteArray): Boolean {
        val envelope = synchronized(stateLock) {
            val state = syncedState ?: return false
            val written = try {
                state.setChannelAdmissionSeed(Hlc.mint(), channelId, version, seed)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "channel admission seed refused (${cause.javaClass.simpleName})")
                false
            }
            if (!written) return false
            runCatching { state.channelAdmissionSeedEnvelopeFor(channelId, version, seed) }.getOrNull()
        }
        // The register is part of the persisted synced state and the backup:
        // the version bump persists it and marks the backup due.
        republish()
        if (envelope != null) {
            scope.launchBound(sessionBinding) {
                try {
                    sendServiceEnvelope(envelope)
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (cause: Throwable) {
                    Log.w(LOG_TAG, "channel admission seed not published (${cause.javaClass.simpleName})")
                    pendingFlag?.recordFailure()
                }
            }
        }
        return true
    }

    /** Every admission seed of [channelId] the register holds, each with its
     *  version (a version may appear more than once); empty while the state
     *  is not open. The caller wipes the seeds. */
    public fun channelAdmissionSeeds(channelId: ByteArray): List<Pair<UShort, ByteArray>> =
        synchronized(stateLock) {
            val state = syncedState ?: return emptyList()
            runCatching { state.channelAdmissionSeeds(channelId) }.getOrNull().orEmpty()
                .map { it.admissionVersion to it.seed }
        }

    /** A received admission seed envelope, folded by its own stamp. */
    private fun applyChannelAdmissionSeed(envelope: ByteArray) {
        val changed = synchronized(stateLock) {
            val state = syncedState ?: return
            try {
                state.applyChannelAdmissionSeedEnvelope(envelope)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "channel admission seed envelope refused (${cause.javaClass.simpleName})")
                false
            }
        }
        // Persisted and in the backup like every other register.
        if (changed) republish()
    }

    /** ADR-0090 section 2.1 - the signature keys of the account's DeviceGroup
     *  roster (the authority on which devices belong to the account), or
     *  `null` while no DeviceGroup is open. */
    public fun rosterSignatureKeys(): List<ByteArray>? {
        val g = group.get() ?: return null
        return runCatching { g.devices().map { it.signaturePublicKey } }.getOrNull()
    }

    /** A received channel subscription envelope, folded by its own stamp. */
    private fun applyChannelSubscription(envelope: ByteArray) {
        val changed = synchronized(stateLock) {
            val state = syncedState ?: return
            try {
                state.applyChannelSubscriptionEnvelope(envelope)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "channel subscription envelope refused (${cause.javaClass.simpleName})")
                false
            }
        }
        if (changed) republish()
    }

    /** Fold one local link-record write, then send that record to the
     *  account's other devices. A failed publish is kept by the pending
     *  flag and re-sent with the catch-up. */
    private suspend fun writeLinkRecord(
        linkId: ByteArray,
        mustPersist: Boolean = false,
        sendInBackground: Boolean = false,
        write: (SyncedState) -> Boolean,
    ): Boolean {
        val envelope = synchronized(stateLock) {
            val state = syncedState ?: return false
            val written = try {
                write(state)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "link record refused (${cause.javaClass.simpleName})")
                false
            }
            if (!written) return false
            runCatching { state.linkRecordEnvelopeFor(linkId) }.getOrNull()
        }
        republish()
        // On disk at once rather than after the debounce; a link whose record
        // could not be written is not handed out (the caller revokes it).
        if (!syncStatePersistence.writeNow() && mustPersist) return false
        if (envelope != null) {
            val send: suspend () -> Unit = {
                try {
                    sendServiceEnvelope(envelope)
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (cause: Throwable) {
                    Log.w(LOG_TAG, "link record not published (${cause.javaClass.simpleName})")
                    pendingFlag?.recordFailure()
                }
            }
            // A copy mark goes out in the background, so Copy does not wait
            // on the network (as iOS and web); a failed send is retried like
            // any other unsent link record.
            if (sendInBackground) scope.launchBound(sessionBinding) { send() } else send()
        }
        return true
    }

    /** Record a link just minted. `false` when the state is not open or the
     *  record is refused. */
    public suspend fun setLinkRecord(record: LinkRecord): Boolean =
        writeLinkRecord(record.linkId, mustPersist = true) { it.setLinkRecord(Hlc.mint(), record) }

    /** Mark a link revoked (final). `false` when nothing changed. */
    public suspend fun revokeLinkRecord(linkId: ByteArray): Boolean =
        writeLinkRecord(linkId) { it.revokeLinkRecord(Hlc.mint(), linkId) }

    /** Mark a link copied (final, ADR-0079 amendment 2026-10-26): from then
     *  on no device of the account offers to edit it. `false` when nothing
     *  changed (already copied) or the state is not open. */
    public suspend fun markLinkShared(linkId: ByteArray): Boolean =
        writeLinkRecord(linkId, sendInBackground = true) { it.markLinkShared(Hlc.mint(), linkId) }

    /** Count one request accepted through a link, remembering the requester. */
    public suspend fun recordLinkUse(linkId: ByteArray, requesterKey: ByteArray): Boolean =
        writeLinkRecord(linkId) { it.recordLinkUse(Hlc.mint(), linkId, requesterKey) }

    /**
     * The one rule for a request's link answer (ADR-0079 section 3 as
     * amended 2026-10-19), asked of the fold: the link's expiry is judged at
     * [pressedAtMs] (the time the DS stamped on the request row), its
     * revocation and uses now. `null` while the fold is not loaded: the
     * request stays undecided (no timer) and is decided once [ready] turns
     * `true`.
     */
    public fun linkAnswerVerdictAt(
        linkId: ByteArray,
        linkSecret: ByteArray,
        requesterKey: ByteArray,
        pressedAtMs: ULong,
    ): LinkAnswerVerdict? {
        if (!ready.value) return null
        return synchronized(stateLock) {
            val state = syncedState ?: return null
            runCatching {
                state.linkAnswerVerdict(linkId, linkSecret, requesterKey, pressedAtMs)
            }.getOrDefault(LinkAnswerVerdict.DEAD)
        }
    }

    private val mutableLinkClaimsVersion: MutableStateFlow<Long> = MutableStateFlow(0L)

    /** ADR-0079 (reworked 2026-10-17) - bumped whenever a claim of a link
     *  request is recorded (another device's, or this device's own row
     *  recognised by its bytes). */
    public val linkClaimsVersion: StateFlow<Long> = mutableLinkClaimsVersion.asStateFlow()

    /**
     * ADR-0079 (reworked 2026-10-17) - seal and post this device's claim of
     * a link request into the account's DeviceGroup, on its own
     * `ServiceEnvelope` carrier. [beforePublish] gets the sealed bytes (hex)
     * and must record them durably (`false` aborts the publish). The publish
     * answer files nothing: the receive loop judges the row at its position
     * ([judgeOwnLinkRow]; it counts only when sealed at the journal's epoch
     * there). The snapshot is persisted before the publish, as for every
     * service envelope. Returns the DS message id.
     */
    public suspend fun postLinkClaim(
        linkId: ByteArray,
        requesterKey: ByteArray,
        beforePublish: (String) -> Boolean,
    ): Long =
        publishRecorded(uniffi.lakko_ffi_uniffi.composeLinkClaimEnvelope(linkId, requesterKey), beforePublish)

    private suspend fun publishRecorded(envelope: ByteArray, beforePublish: (String) -> Boolean): Long =
        onHandleCaughtUp {
            check(!mutableDeviceRemoved.value) { "this device was removed from the account" }
            val g = group.get() ?: error("DeviceGroupSync not bootstrapped")
            val id = identity.get() ?: error("DeviceGroupSync not bootstrapped")
            val accountIdB64 = activeAccountIdB64.get() ?: error("DeviceGroupSync not bootstrapped")
            val wireBytes = g.createServiceEnvelopeMessage(id, envelope, Hlc.mint())
            check(beforePublish(me.lakko.app.contacts.LinkClaims.hex(wireBytes))) { "link row not recorded" }
            persistHandleOrThrow(id, accountIdB64)
            DsClient().use { ds ->
                ds.publishGroup(g.groupId(), DeviceGroupJournalEnvelope.encode(wireBytes, null)).id
            }
        }

    /** ADR-0079 - this device's DeviceGroup leaf key and the current
     *  roster's leaf keys, the inputs of the claim decision. `null` while no
     *  handle is held or this device was removed. */
    public fun linkClaimRoster(): Pair<ByteArray, List<ByteArray>>? {
        if (mutableDeviceRemoved.value) return null
        val g = group.get() ?: return null
        val devices = runCatching { g.devices() }.getOrNull()?.takeIf { it.isNotEmpty() } ?: return null
        val ownLeaf = runCatching { g.ownLeafIndex() }.getOrNull() ?: return null
        val ownKey = devices.firstOrNull { it.leafIndex == ownLeaf }?.signaturePublicKey ?: return null
        return ownKey to devices.map { it.signaturePublicKey }
    }

    /** ADR-0079 (order of 2026-10-17) - the journal's epochs for one batch:
     *  the record kept next to the claim record, or one starting at the
     *  group's epoch when none is held for this DeviceGroup. */
    private fun loadJournalEpochs(g: DeviceGroup?): Pair<String, uniffi.lakko_ffi_uniffi.FfiJournalEpochs>? {
        val group = g ?: return null
        val accountIdB64 = activeAccountIdB64.get() ?: return null
        val groupIdHex = runCatching { me.lakko.app.contacts.LinkClaims.hex(group.groupId()) }.getOrNull() ?: return null
        val stored = me.lakko.app.contacts.LinkClaimRecord.onDevice(applicationContext, accountIdB64).journal(groupIdHex)
        if (stored != null) {
            return groupIdHex to uniffi.lakko_ffi_uniffi.FfiJournalEpochs(
                stored.base.toULong(),
                stored.commitRowIds.map { it.toULong() },
                stored.commitEpochsAfter.map { it.toULong() },
            )
        }
        val epoch = runCatching { group.epoch() }.getOrNull() ?: return null
        val fresh = uniffi.lakko_ffi_uniffi.journalEpochsStartingAt(epoch)
        // Kept at once: the start must not move with the group's epoch on
        // later batches (an own commit merged early would then make an own
        // claim sealed before it look straddled).
        me.lakko.app.contacts.LinkClaimRecord.onDevice(applicationContext, accountIdB64).setJournal(
            me.lakko.app.contacts.StoredJournal(groupIdHex, fresh.base.toLong(), emptyList(), emptyList()),
        )
        return groupIdHex to fresh
    }

    /** ADR-0079 - a journal row carrying one of this device's recorded
     *  sealed claims or notes (its own application message cannot be
     *  decrypted here) is judged at its position: it counts only when
     *  sealed at the journal's epoch there. `true` when it was one. */
    private fun judgeOwnLinkRow(
        mlsMessage: ByteArray,
        messageId: Long,
        journal: uniffi.lakko_ffi_uniffi.FfiJournalEpochs,
    ): Boolean {
        val accountIdB64 = activeAccountIdB64.get() ?: return false
        val record = me.lakko.app.contacts.LinkClaimRecord.onDevice(applicationContext, accountIdB64)
        if (!record.hasUnmatchedWires()) return false
        val counts = uniffi.lakko_ffi_uniffi.epochOfMessage(mlsMessage)?.let {
            uniffi.lakko_ffi_uniffi.journalRowCounts(journal, messageId.coerceAtLeast(0).toULong(), it)
        } ?: false
        record.judgeOwnRow(me.lakko.app.contacts.LinkClaims.hex(mlsMessage), messageId, counts) ?: return false
        mutableLinkClaimsVersion.value = mutableLinkClaimsVersion.value + 1
        return true
    }

    /** ADR-0079 - after a row was processed (or failed): a commit that took
     *  effect moves the journal's epoch, and the record is kept at once (it
     *  only ever follows the journal's content). Returns the record. */
    private fun observeJournalRow(
        mlsMessage: ByteArray,
        messageId: Long,
        g: DeviceGroup,
        journal: Pair<String, uniffi.lakko_ffi_uniffi.FfiJournalEpochs>?,
    ): Pair<String, uniffi.lakko_ffi_uniffi.FfiJournalEpochs>? {
        val (groupIdHex, epochs) = journal ?: return null
        val sealed = uniffi.lakko_ffi_uniffi.epochOfMessage(mlsMessage) ?: return journal
        val after = uniffi.lakko_ffi_uniffi.epochAfterMessage(mlsMessage) ?: return journal
        if (after <= sealed) return journal
        val groupEpoch = runCatching { g.epoch() }.getOrNull() ?: return journal
        val next = uniffi.lakko_ffi_uniffi.journalEpochsObserve(
            epochs,
            messageId.coerceAtLeast(0).toULong(),
            sealed,
            after,
            groupEpoch,
        )
        if (next == epochs) return journal
        activeAccountIdB64.get()?.let { accountIdB64 ->
            me.lakko.app.contacts.LinkClaimRecord.onDevice(applicationContext, accountIdB64).setJournal(
                me.lakko.app.contacts.StoredJournal(
                    groupIdHex = groupIdHex,
                    base = next.base.toLong(),
                    commitRowIds = next.commitRowIds.map { it.toLong() },
                    commitEpochsAfter = next.commitEpochsAfter.map { it.toLong() },
                ),
            )
        }
        return groupIdHex to next
    }

    /** Wait until a poll that started after this call read the DeviceGroup
     *  journal to its head, so every row up to a claim just published has
     *  been applied. `false` when that did not happen in time. */
    public suspend fun awaitJournalReadPastNow(timeoutMs: Long = CLAIM_HEAD_TIMEOUT_MS): Boolean =
        awaitHead(timeoutMs)

    /**
     * ADR-0079 (reworked 2026-10-17) - another device's claim of a link
     * request, taken in. Honoured only from a sender leaf the roster this
     * row was sealed under resolves (C-68); its DS id is kept with that
     * leaf's key. `true` when the envelope was a claim (kept or dropped).
     */
    /**
     * ADR-0079 (order of 2026-10-17) - publish the link-founded note (the
     * request, the one-to-one founded for it, its Welcome) into the
     * account's DeviceGroup, BEFORE any Welcome is written. Returns the DS
     * message id; throws when the publish failed. The snapshot is persisted
     * before the publish, as for every service envelope.
     */
    public suspend fun postLinkFounded(
        linkId: ByteArray,
        requesterKey: ByteArray,
        groupId: ByteArray,
        welcome: ByteArray,
        beforePublish: (String) -> Boolean,
    ): Long =
        publishRecorded(
            uniffi.lakko_ffi_uniffi.composeLinkFoundedEnvelope(linkId, requesterKey, groupId, welcome),
            beforePublish,
        )

    /**
     * ADR-0079 - another device's link-founded note, taken in. Kept (the
     * earliest per request) only from a sender leaf the roster this row was
     * sealed under resolves (C-68). `true` when the envelope was a note (kept
     * or dropped).
     */
    private fun receiveLinkFounded(envelope: ByteArray, senderLeaf: UInt?, g: DeviceGroup, messageId: Long): Boolean {
        if (!uniffi.lakko_ffi_uniffi.isLinkFoundedEnvelope(envelope)) return false
        val note = try {
            uniffi.lakko_ffi_uniffi.decodeLinkFoundedEnvelope(envelope)
        } catch (_: Throwable) {
            return true
        }
        val leaf = senderLeaf ?: return true
        val roster = runCatching { g.devices() }.getOrNull() ?: return true
        if (roster.none { it.leafIndex == leaf }) return true
        val accountIdB64 = activeAccountIdB64.get() ?: return true
        val changed = me.lakko.app.contacts.LinkClaimRecord.onDevice(applicationContext, accountIdB64)
            .recordNote(
                me.lakko.app.contacts.LinkClaims.requestKeyHex(note.linkId, note.requesterAccountKey),
                me.lakko.app.contacts.LinkClaims.HeldNote(
                    id = messageId,
                    groupIdHex = DsClient.pseudonymHex(note.groupId),
                    welcomeB64 = java.util.Base64.getEncoder().encodeToString(note.welcome),
                ),
                System.currentTimeMillis(),
            )
        if (changed) mutableLinkClaimsVersion.value = mutableLinkClaimsVersion.value + 1
        return true
    }

    private fun receiveLinkClaim(envelope: ByteArray, senderLeaf: UInt?, g: DeviceGroup, messageId: Long): Boolean {
        if (!uniffi.lakko_ffi_uniffi.isLinkClaimEnvelope(envelope)) return false
        val claim = try {
            uniffi.lakko_ffi_uniffi.decodeLinkClaimEnvelope(envelope)
        } catch (_: Throwable) {
            return true
        }
        val leaf = senderLeaf ?: return true
        val roster = runCatching { g.devices() }.getOrNull() ?: return true
        val sender = roster.firstOrNull { it.leafIndex == leaf } ?: return true
        val accountIdB64 = activeAccountIdB64.get() ?: return true
        val changed = me.lakko.app.contacts.LinkClaimRecord.onDevice(applicationContext, accountIdB64)
            .recordSeen(
                me.lakko.app.contacts.LinkClaims.requestKeyHex(claim.linkId, claim.requesterAccountKey),
                messageId,
                me.lakko.app.contacts.LinkClaims.hex(sender.signaturePublicKey),
                System.currentTimeMillis(),
            )
        if (changed) mutableLinkClaimsVersion.value = mutableLinkClaimsVersion.value + 1
        return true
    }

    /** A received link-record envelope, folded by its original stamp. */
    private fun applyLinkRecord(envelope: ByteArray) {
        val changed = synchronized(stateLock) {
            val state = syncedState ?: return
            try {
                state.applyLinkRecordEnvelope(envelope)
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "link record envelope refused (${cause.javaClass.simpleName})")
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
        // reactions-and-emoji D3 - and the quick reaction.
        runCatching { restored.quickReactionEnvelope() }.getOrNull()?.let { applyQuickReaction(it) }
        // UX 17.6 - and the device acknowledgements.
        for (envelope in runCatching { restored.deviceAckEnvelopes() }.getOrNull().orEmpty()) applyDeviceAck(envelope)
        // ADR-0079 - and the link records.
        for (envelope in runCatching { restored.linkRecordEnvelopes() }.getOrNull().orEmpty()) applyLinkRecord(envelope)
        // docs/ux/second-device.md section 4a - and the channel subscriptions.
        for (envelope in runCatching { restored.channelSubscriptionEnvelopes() }.getOrNull().orEmpty()) {
            applyChannelSubscription(envelope)
        }
        // ADR-0090 - and the owner account's channel admission seeds.
        for (envelope in runCatching { restored.channelAdmissionSeedEnvelopes() }.getOrNull().orEmpty()) {
            applyChannelAdmissionSeed(envelope)
        }
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
    /** ADR-0065 section 3 — every blob id the saved list names, for the keep
     *  tick; empty before the state is open. */
    public fun savedBlobIds(): List<ByteArray> = synchronized(stateLock) {
        try {
            syncedState?.savedBlobIds().orEmpty()
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "saved blob ids not read", cause)
            emptyList()
        }
    }

    /**
     * ADR-0065 section 4 — save or remove [registers], in change sets of at
     * most `savedMaxPerChangeSet()` registers each (one DeviceGroup message
     * stays small), applied here at once and sent to the other devices.
     */
    public suspend fun applySavedChanges(registers: List<uniffi.lakko_ffi_uniffi.SavedMessageRegister>) {
        val per = uniffi.lakko_ffi_uniffi.savedMaxPerChangeSet().toInt().coerceAtLeast(1)
        for (chunk in registers.chunked(per)) {
            applyFolderChanges(
                FolderChangeSet(
                    metas = emptyList(),
                    members = emptyList(),
                    pins = emptyList(),
                    removed = emptyList(),
                    order = null,
                    saved = chunk,
                ),
            )
        }
    }

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
        onHandleCaughtUp {
            check(!mutableDeviceRemoved.value) { "this device was removed from the account" }
            val g = group.get() ?: error("DeviceGroupSync not bootstrapped")
            val id = identity.get() ?: error("DeviceGroupSync not bootstrapped")
            val accountIdB64 = activeAccountIdB64.get()
                ?: error("DeviceGroupSync not bootstrapped")
            val wireBytes = g.createDevicegroupMessage(id, body, hlc)
            val groupId = g.groupId()
            // ADR-0081 decision 3 — the ratchet step this message took is on disk
            // before the ciphertext is published, or nothing is published.
            persistHandleOrThrow(id, accountIdB64)
            // G-13 (ADR-0015): wrap in the journal envelope (application messages
            // carry no attestation).
            val envelope = DeviceGroupJournalEnvelope.encode(wireBytes, null)
            DsClient().use { ds ->
                ds.publishGroup(groupId, envelope)
            }
        }
    }

    /**
     * Run one send, and once more when the held handle turned out to be behind
     * the disk ([DeviceGroupSnapshotStore.SnapshotChanged]: another handle of
     * this device wrote since). The first attempt sealed on the stale handle
     * and nothing of it was published, so the retry on the reopened handle
     * reuses no ratchet position. Not cancellable, because the reopen cancels
     * the poll job a send may be running in.
     */
    private suspend fun <T> onHandleCaughtUp(send: suspend () -> T): T =
        retryOnceAfterSnapshotChanged(
            send = send,
            catchUp = { reopenBehindDisk() },
            again = { withContext(NonCancellable) { send() } },
        )

    /** Reopen the held handle from the disk unless another caller already
     *  did. One at a time: two sends can lose the same race. */
    private fun reopenBehindDisk() {
        synchronized(reopenLock) {
            val accountIdB64 = activeAccountIdB64.get() ?: return
            if (handleGeneration == DeviceGroupSnapshotStore.generation(accountIdB64)) return
            reopenAfterLocalChange()
        }
    }

    private val reopenLock = Any()

    /**
     * ADR-0081 decision 3 — persist the held handle, FAIL-CLOSED and
     * conditional on the generation it read. A handle another one wrote past
     * (a commit of this device made N+1 current) makes the write throw: nothing
     * built on the stale handle is published. A send catches up and runs again
     * ([onHandleCaughtUp]); the poll reopens and reads on.
     */
    private fun persistHandleOrThrow(id: Identity, accountIdB64: String) {
        handleGeneration = DeviceGroupLifecycle.persistSnapshotOrThrow(
            context = applicationContext,
            identity = id,
            accountIdB64 = accountIdB64,
            expectedGeneration = handleGeneration,
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
        // reactions-and-emoji D3 - nor is the quick reaction.
        val quickReactionEnvelope = synchronized(stateLock) {
            runCatching { syncedState?.quickReactionEnvelope() }.getOrNull()
        }
        quickReactionEnvelope?.let { sendServiceEnvelope(it) }
        // UX 17.6 - nor are the device acknowledgements: this is also how an
        // acknowledgement whose live send failed is re-sent.
        val deviceAckEnvelopes = synchronized(stateLock) {
            runCatching { syncedState?.deviceAckEnvelopes() }.getOrNull().orEmpty()
        }
        for (envelope in deviceAckEnvelopes) sendServiceEnvelope(envelope)
        // ADR-0092 section 3.6 - nor are the own poll choices.
        val pollChoiceEnvelopes = synchronized(stateLock) {
            runCatching { syncedState?.pollChoiceEnvelopes() }.getOrNull().orEmpty()
        }
        for (envelope in pollChoiceEnvelopes) sendServiceEnvelope(envelope)
        // ADR-0079 - nor are the link records; each goes with its own stamp.
        val linkRecordEnvelopes = synchronized(stateLock) {
            runCatching { syncedState?.linkRecordEnvelopes() }.getOrNull().orEmpty()
        }
        for (envelope in linkRecordEnvelopes) sendServiceEnvelope(envelope)
        // ADR-0087 section 13a - nor are the own channel reactions and comments.
        val feedbackEnvelopes = synchronized(stateLock) {
            runCatching { syncedState?.channelReactionEnvelopes() }.getOrNull().orEmpty() +
                runCatching { syncedState?.channelCommentEnvelopes() }.getOrNull().orEmpty()
        }
        for (envelope in feedbackEnvelopes) sendServiceEnvelope(envelope)
        // docs/ux/second-device.md section 4a - nor are the channel
        // subscription registers, each with its own stamp.
        val channelSubscriptionEnvelopes = synchronized(stateLock) {
            runCatching { syncedState?.channelSubscriptionEnvelopes() }.getOrNull().orEmpty()
        }
        for (envelope in channelSubscriptionEnvelopes) sendServiceEnvelope(envelope)
        // ADR-0090 - nor are the channel admission seeds, each with its own
        // stamp (re-sent on growth like the subscriptions).
        val admissionSeedEnvelopes = synchronized(stateLock) {
            runCatching { syncedState?.channelAdmissionSeedEnvelopes() }.getOrNull().orEmpty()
        }
        for (envelope in admissionSeedEnvelopes) sendServiceEnvelope(envelope)
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
        // ADR-0080 - the hand-over: the stored own card at once on the
        // post-add handle (this device never sees its own add commit come
        // back, so no growth timer is needed for the device it let in).
        me.lakko.app.devices.DeviceCardCenter.sendHandOver()
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
     * Read the DeviceGroup journal until cancelled, the handle is gone, this
     * device is removed, or it is behind for good (ADR-0081 decision 4: it
     * then rejoins through a sibling).
     */
    private suspend fun runPollLoop(
        bound: me.lakko.app.Binding,
        accountIdBytes: ByteArray,
        accountIdB64: String,
        groupId: ByteArray,
        startCursor: Long = 0L,
    ) {
        // Resume where the last persisted batch left off instead of re-reading
        // the whole journal on every start (DeviceGroupJournalCursor).
        val storedCursor = runCatching {
            DeviceGroupJournalCursor.read(applicationContext, accountIdB64, groupId)
        }.getOrDefault(0L)
        var cursor = maxOf(startCursor, storedCursor)
        val durableCursor = JournalCursorTracker(cursor)
        // ADR-0081 third amendment, point 5 — signed reads refused in a row.
        var refusals = 0
        DsClient().use { ds ->
            while (currentCoroutineContextActive()) {
                val result = try {
                    fetchOrKicked(ds, groupId, cursor) ?: continue
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (refused: me.lakko.app.ds.DsProofRefusedError) {
                    if (refused.clockSkewed) {
                        Log.w(LOG_TAG, "poll fetch refused: the device clock is off")
                        delay(5000)
                        continue
                    }
                    if (DeviceGroupJournalAccess.isFoundingUnconfirmed(applicationContext, accountIdB64)) {
                        // This device's own founding is not claimed yet: claim
                        // it (a root reset over the chain a gone group left).
                        Log.w(LOG_TAG, "poll fetch refused before the founding was claimed; claiming")
                        claimJournalBestEffort(groupId, refounded = true)
                        delay(5000)
                        continue
                    }
                    refusals += 1
                    Log.w(LOG_TAG, "poll fetch refused ($refusals in a row)")
                    val held = group.get() ?: return
                    if (refusals >= REFUSALS_BEFORE_LOST &&
                        RemovedDevice.rejoinAllowed(RemovedDevice.readActive { held.isActive() })
                    ) {
                        // The journal's chain is not this device's: it was
                        // founded again while this device was not running.
                        Log.w(LOG_TAG, "DeviceGroup journal refuses this device's chain; rejoining")
                        enterRejoin(accountIdBytes, accountIdB64)
                        return
                    }
                    delay(5000)
                    continue
                } catch (http: DsHttpError) {
                    if (http.status == 404) continue
                    Log.w(LOG_TAG, "poll fetch http ${http.status}")
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

                refusals = 0
                // The fetch answered, so the network works: send whatever of
                // ours is still waiting.
                flushPendingIfAny()
                // ADR-0080 - an own device card that failed or came due in
                // the background goes out now (only while the app shows).
                me.lakko.app.devices.DeviceCardCenter.sendOwedIfAny()
                val drainedBatch = result.messages.isEmpty() && !result.hasMore

                // ADR-0081 decision 2, step 6 — a DeviceGroup commit of this
                // device that did not finish (the DS could not be reached, a
                // write failed, a crash) is finished before anything else;
                // until it is, this device never reads itself as behind (the
                // DS may hold its own next epoch). Not cancellable: a landed
                // entry reopens this runtime (which cancels this loop), and
                // its Welcome must still be delivered.
                var commitPending = false
                try {
                    val owed = withContext(NonCancellable) {
                        me.lakko.app.devices.DeviceGroupCommits.resendOwed(
                            applicationContext, accountIdBytes, accountIdB64,
                        )
                    }
                    if (owed != null) Log.i(LOG_TAG, "DeviceGroup commit outbox: ${owed.javaClass.simpleName}")
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (cause: Throwable) {
                    commitPending = true
                    Log.w(LOG_TAG, "DeviceGroup commit outbox: not finished yet (${cause.javaClass.simpleName})")
                }
                if (!currentCoroutineContextActive() || !bound.isCurrent) return

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
                // ADR-0079 (order of 2026-10-17) - the journal readability
                // rule for this batch: own claims and notes judged at their
                // rows, commits moving the journal's epoch.
                var journal = loadJournalEpochs(group.get())
                // This device's own rows coming back, counted for one line per
                // batch: each is benign (its effect is already in the
                // snapshot), and a stack per row flooded the log.
                var ownRows = 0
                for (message in result.messages) {
                    // The account this loop polls for is gone: the handles
                    // read below are nobody's, or the next account's.
                    if (!bound.isCurrent) return
                    cursor = message.id
                    val g = group.get() ?: return
                    val id = identity.get() ?: return
                    // A commit earlier in this batch removed this device: the
                    // group is frozen, nothing after it decrypts. Stop reading
                    // the batch; the check after the persist below stops the loop.
                    if (RemovedDevice.isRemoved(RemovedDevice.readActive { g.isActive() })) break
                    // Unwrap the journal envelope (MLS message + any attestation).
                    val entry = try {
                        DeviceGroupJournalEnvelope.decode(message.payload)
                    } catch (cause: Throwable) {
                        // No identifier: the row id and the cause's text are left out.
                        Log.w(LOG_TAG, "journal envelope decode failed (${cause.javaClass.simpleName})")
                        // Never readable by any later pass either.
                        durableCursor.passed(message.id)
                        continue
                    }
                    // ADR-0079 - this device's own claim or note, recognised
                    // by the sealed bytes it recorded before the publish and
                    // judged at this row's position; recorded before the
                    // cursor moves past it.
                    if (journal != null && judgeOwnLinkRow(entry.first, message.id, journal.second)) {
                        durableCursor.passed(message.id)
                        continue
                    }
                    val attestations: List<ByteArray> =
                        entry.second?.let { listOf(it) } ?: emptyList()
                    val devicesBeforeRow = runCatching { g.devices().map { it.signaturePublicKey } }.getOrNull()
                    val epochBeforeRow = runCatching { g.epoch() }.getOrNull()
                    val processed: ProcessedMessage = try {
                        g.processIncomingVerifyingRemovals(
                            id,
                            entry.first,
                            attestations,
                            attestVk,
                            System.currentTimeMillis() / 1000,
                        )
                    } catch (cause: Throwable) {
                        val own = me.lakko.app.push.isOwnMessageEcho(cause)
                        val verdict = failedRowVerdict(
                            isOwnEcho = own,
                            rowId = message.id,
                            localEpoch = epochBeforeRow,
                            registeredCommits = result.registeredCommits,
                        )
                        if (own) {
                            ownRows += 1
                        } else {
                            // ADR-0091: no identifier, the failure kind and
                            // the verdict only.
                            Log.w(
                                LOG_TAG,
                                "DeviceGroup journal row not applied: kind=" +
                                    me.lakko.app.chat.failedRowKind(cause) +
                                    " verdict=" + (if (verdict == FailedRowVerdict.HOLD) "hold" else "skip"),
                            )
                        }
                        durableCursor.refused(message.id, verdict)
                        // A commit that took effect (its own, merged early)
                        // still moves the journal's epoch.
                        journal = observeJournalRow(entry.first, message.id, g, journal)
                        continue
                    }
                    journal = observeJournalRow(entry.first, message.id, g, journal)
                    durableCursor.passed(message.id)
                    mutated = true
                    if (processed is ProcessedMessage.Application) {
                        try {
                            val parsed = parseDevicegroupPayload(
                                processed.`data`,
                            )
                            val body = parsed.body
                            if (body is DeviceGroupBody.ServiceEnvelope &&
                                uniffi.lakko_ffi_uniffi.isPollChoiceEnvelope(body.envelope)
                            ) {
                                // ADR-0092 section 3.6 - this account's own
                                // poll choice from another of its devices.
                                applyPollChoice(body.envelope)
                            } else if (body is DeviceGroupBody.ServiceEnvelope &&
                                uniffi.lakko_ffi_uniffi.isDeviceAckEnvelope(body.envelope)
                            ) {
                                // UX 17.6 - another device of the account saw
                                // a new device's warning; a `lakko-sync`
                                // envelope, routed before the lakko-contacts
                                // dispatcher.
                                applyDeviceAck(body.envelope)
                            } else if (body is DeviceGroupBody.ServiceEnvelope &&
                                uniffi.lakko_ffi_uniffi.isLinkRecordEnvelope(body.envelope)
                            ) {
                                // ADR-0079 - one of the account's invite links
                                // from another of its devices.
                                applyLinkRecord(body.envelope)
                            } else if (body is DeviceGroupBody.ServiceEnvelope &&
                                uniffi.lakko_ffi_uniffi.isChannelSubscriptionEnvelope(body.envelope)
                            ) {
                                // docs/ux/second-device.md section 4a - a
                                // channel the account subscribed to or left on
                                // another of its devices.
                                applyChannelSubscription(body.envelope)
                            } else if (body is DeviceGroupBody.ServiceEnvelope &&
                                uniffi.lakko_ffi_uniffi.isChannelAdmissionSeedEnvelope(body.envelope)
                            ) {
                                // ADR-0090 - an admission seed another owner
                                // device of the account minted or holds.
                                applyChannelAdmissionSeed(body.envelope)
                            } else if (body is DeviceGroupBody.ServiceEnvelope &&
                                uniffi.lakko_ffi_uniffi.isChannelReactionEnvelope(body.envelope)
                            ) {
                                // ADR-0087 section 13a - an own channel reaction.
                                applyChannelFeedback(body.envelope, reaction = true)
                            } else if (body is DeviceGroupBody.ServiceEnvelope &&
                                uniffi.lakko_ffi_uniffi.isChannelCommentEnvelope(body.envelope)
                            ) {
                                // ADR-0087 section 13a - an own channel comment.
                                applyChannelFeedback(body.envelope, reaction = false)
                            } else if (body is DeviceGroupBody.ServiceEnvelope &&
                                uniffi.lakko_ffi_uniffi.isQuickReactionEnvelope(body.envelope)
                            ) {
                                // reactions-and-emoji D3 - the account's quick
                                // reaction from another of its devices.
                                applyQuickReaction(body.envelope)
                            } else if (body is DeviceGroupBody.ServiceEnvelope &&
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
                            } else if (body is DeviceGroupBody.ServiceEnvelope && isConversationReentry(body.envelope)) {
                                // ADR-0091 phase 2 route 1 - another device of
                                // this account fell behind in a conversation
                                // and asks to be brought back. The chat layer
                                // runs every check; the sender is resolved
                                // here, against the roster this row was sealed
                                // under (C-68).
                                val senderKey = processed.senderLeaf?.let { leaf ->
                                    runCatching {
                                        g.devices().firstOrNull { it.leafIndex == leaf }?.signaturePublicKey
                                    }.getOrNull()
                                }
                                conversationReentrySink?.invoke(body.envelope, senderKey)
                            } else if (body is DeviceGroupBody.ServiceEnvelope && isKeyRotation(body.envelope)) {
                                // ADR-0074 section 3.3 - the account's new
                                // keys from the device that changed them.
                                // Meaningful ONLY here, in the account's own
                                // DeviceGroup; verified and stored as
                                // pending by the auth side, never rendered.
                                keyRotationSink?.invoke(body.envelope)
                            } else if (body is DeviceGroupBody.ServiceEnvelope &&
                                receiveDeviceCard(body.envelope, processed.senderLeaf, g)
                            ) {
                                // ADR-0080 - a device card, kept under the
                                // sender's leaf key resolved here, against the
                                // roster this row was sealed under (C-68).
                            } else if (body is DeviceGroupBody.ServiceEnvelope &&
                                receiveLinkFounded(body.envelope, processed.senderLeaf, g, message.id)
                            ) {
                                // ADR-0079 (order of 2026-10-17) - another
                                // device's link-founded note, resolved the
                                // same way.
                            } else if (body is DeviceGroupBody.ServiceEnvelope &&
                                receiveLinkClaim(body.envelope, processed.senderLeaf, g, message.id)
                            ) {
                                // ADR-0079 (reworked 2026-10-17) - another
                                // device's claim of a link request, resolved
                                // the same way.
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
                        // ADR-0081 decision 4 — the sibling's record of which
                        // keys it saw added, and when (the row's DS time), so a
                        // knock another sibling answered is not answered twice.
                        noteAdditions(devicesBeforeRow, g, message.createdAt)
                        val after = runCatching { g.devices().map { it.leafIndex }.toSet() }.getOrNull()
                        if (after != null) {
                            SyncCatchUp.priorLeavesIfGrown(knownLeaves, after)?.let { prior ->
                                // ADR-0080 - every device that was there
                                // re-sends its own card.
                                me.lakko.app.devices.DeviceCardCenter.scheduleResend()
                                onGrowth(prior)
                            }
                            knownLeaves = after
                        }
                        // UX 17.5 - another device changed the account's
                        // device list: a device this one has not
                        // acknowledged raises the notice.
                        reportRoster(g, accountIdB64, localChange = false)
                        rosterChange.onCommitApplied()
                    }
                    // Proposal / ExternalJoinProposal — no application-state
                    // mutation.
                }

                if (ownRows > 0) Log.i(LOG_TAG, "skipped $ownRows own rows of this device")
                if (!bound.isCurrent) return
                if (mutated) {
                    val id = identity.get()
                    if (id == null) durableCursor.snapshotPersistFailed()
                    if (id != null) {
                        // ADR-0081 decision 3 — fail-closed and conditional on
                        // the generation this handle read: a handle another one
                        // wrote past reopens instead of putting its older state
                        // back. Any other failure keeps the applied state in
                        // memory; the next batch writes it again.
                        try {
                            persistHandleOrThrow(id, accountIdB64)
                            durableCursor.snapshotPersisted()
                            saveAdmissions(accountIdB64)
                            rosterChange.onPersisted()
                        } catch (_: DeviceGroupSnapshotStore.SnapshotChanged) {
                            scope.launch { reopenBehindDisk() }
                            return
                        } catch (cause: Throwable) {
                            durableCursor.snapshotPersistFailed()
                            Log.w(LOG_TAG, "DeviceGroup snapshot persist failed (${cause.javaClass.simpleName})")
                        }
                    }
                }
                // C-67 - the cursor goes to disk only after the snapshot that
                // reflects every row up to it (DeviceGroupJournalCursor).
                durableCursor.cursorToWrite()?.let { value ->
                    try {
                        DeviceGroupJournalCursor.write(applicationContext, accountIdB64, groupId, value)
                        durableCursor.cursorWritten(value)
                    } catch (cause: Throwable) {
                        Log.w(LOG_TAG, "DeviceGroup journal cursor not stored (${cause.javaClass.simpleName})")
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
                val held = group.get() ?: return
                val step = RemovedDevice.pollStepAfterBatch(RemovedDevice.readActive { held.isActive() })
                if (step == PollStep.StopRemoved) {
                    markRemoved()
                    return
                }

                // ADR-0081 decision 4 — behind for good? A drained read while
                // the DS names a newer epoch than this handle's: since an
                // epoch is registered only with its commit row, nothing left
                // in the journal can bring this device forward. It drops its
                // DeviceGroup state and rejoins through a sibling. A handle
                // that derives no signer (an inactive group) is never behind.
                val localEpoch = currentLocalEpoch()
                if (localEpoch != null &&
                    isBehind(localEpoch, drainedBatch, result.latestEpoch, commitPending) &&
                    RemovedDevice.rejoinAllowed(RemovedDevice.readActive { held.isActive() })
                ) {
                    Log.w(
                        LOG_TAG,
                        "DeviceGroup behind the DS with nothing left to read: local epoch $localEpoch, " +
                            "DS epoch ${result.latestEpoch}; rejoining",
                    )
                    enterRejoin(accountIdBytes, accountIdB64)
                    return
                }
                // ADR-0081 third amendment, point 5 — the DS names an epoch
                // below this handle's: the journal was founded again under it.
                if (localEpoch != null &&
                    isJournalReset(localEpoch, drainedBatch, result.latestEpoch) &&
                    RemovedDevice.rejoinAllowed(RemovedDevice.readActive { held.isActive() })
                ) {
                    Log.w(
                        LOG_TAG,
                        "DeviceGroup journal below this device: local epoch $localEpoch, " +
                            "DS epoch ${result.latestEpoch}; rejoining",
                    )
                    enterRejoin(accountIdBytes, accountIdB64)
                    return
                }
                // At the head: admit the knocks the listener read (ADR-0081
                // decision 4, third amendment: no timer, no designated admitter).
                if (drainedBatch && !commitPending && knocksPending.getAndSet(false)) {
                    try {
                        // Not cancellable: an admission's commits reopen this
                        // runtime (which cancels this loop) and must finish.
                        withContext(NonCancellable) { checkKnocks(accountIdBytes, accountIdB64, bound) }
                    } catch (cancellation: CancellationException) {
                        throw cancellation
                    } catch (cause: Throwable) {
                        knocksPending.set(true)
                        Log.w(LOG_TAG, "DeviceGroup knock check failed (${rejoinFailureCode(cause)})")
                    }
                    if (!bound.isCurrent) return
                }
            }
        }
    }

    /** The epoch of the DeviceGroup handle held now, read from its journal
     *  signer (the FFI `DeviceGroup` has no epoch read), or `null`. */
    private fun currentLocalEpoch(): ULong? {
        val g = group.get() ?: return null
        val id = identity.get() ?: return null
        return DeviceGroupJournalAccess.signerOf(g, id)?.epoch
    }

    // ------------------------------------------------------------
    // ADR-0081 decision 4: founding, rejoining, admitting
    // ------------------------------------------------------------

    /**
     * No usable DeviceGroup state at open. Ask the directory: with no other
     * own device listed, found (nobody can be cut off); otherwise rejoin
     * through a sibling. A directory that cannot be read founds nothing; the
     * question is asked again.
     */
    private fun startFoundOrRejoin(
        accountIdBytes: ByteArray,
        accountIdB64: String,
        deviceSeed: ByteArray,
        seedState: Boolean,
        accountIdentitySeed: ByteArray?,
    ) {
        if (rejoinJob?.isActive == true) {
            deviceSeed.fill(0)
            return
        }
        val seedCopy = deviceSeed.copyOf()
        deviceSeed.fill(0)
        val bound = sessionBinding.capture()
        rejoinJob = scope.launch {
            try {
                var mayFound: Boolean? = null
                while (mayFound == null && currentCoroutineContextActive() && bound.isCurrent) {
                    mayFound = try {
                        directoryListsNoOtherDevice(accountIdBytes, seedCopy)
                    } catch (cancellation: CancellationException) {
                        throw cancellation
                    } catch (cause: Throwable) {
                        Log.w(LOG_TAG, "the directory could not be read; founding nothing (${cause.javaClass.simpleName})")
                        delay(5000)
                        null
                    }
                }
                if (mayFound == null || !bound.isCurrent || activeAccountIdB64.get() != accountIdB64) return@launch
                if (mayFound) {
                    val ensured = try {
                        DeviceGroupLifecycle.ensureDeviceGroup(
                            context = applicationContext,
                            accountIdBytes = accountIdBytes,
                            accountIdB64 = accountIdB64,
                            deviceIdentitySeed = seedCopy,
                            found = true,
                        )
                    } catch (cancellation: CancellationException) {
                        throw cancellation
                    } catch (cause: Throwable) {
                        Log.w(LOG_TAG, "founding failed; retried next session (${cause.javaClass.simpleName})")
                        return@launch
                    }
                    Log.i(LOG_TAG, "no other device listed; DeviceGroup founded")
                    me.lakko.app.backup.BackupRestoreStore.recordRejoinAnswer(
                        applicationContext, accountIdB64, me.lakko.app.backup.RestoreRejoinAnswer.FOUNDED,
                    )
                    rejoinJob = null
                    adopt(ensured, accountIdBytes, accountIdB64, seedState, accountIdentitySeed, 0L, false)
                    return@launch
                }
                Log.i(LOG_TAG, "no DeviceGroup state while another device is listed; rejoining")
                runRejoin(accountIdBytes, accountIdB64, bound, seedState, accountIdentitySeed)
            } finally {
                seedCopy.fill(0)
            }
        }
    }

    /** Stop following the DeviceGroup, drop its local state, and rejoin.
     *  Called from inside the poll loop, which returns right after. */
    private fun enterRejoin(accountIdBytes: ByteArray, accountIdB64: String) {
        if (mutableDeviceRemoved.value || activeAccountIdB64.get() != accountIdB64) return
        if (rejoinJob?.isActive == true) return
        pollJob = null
        identity.set(null)
        group.set(null)
        handleGeneration = null
        admissions = null
        mutableReady.value = false
        val bound = sessionBinding.capture()
        rejoinJob = scope.launch {
            runRejoin(accountIdBytes, accountIdB64, bound, seedState = false, accountIdentitySeed = null)
        }
    }

    /** The device's side ([rejoinOnce]) until joined or the session ends; then
     *  the joined DeviceGroup is opened like any session start. */
    private suspend fun runRejoin(
        accountIdBytes: ByteArray,
        accountIdB64: String,
        bound: me.lakko.app.Binding,
        seedState: Boolean,
        accountIdentitySeed: ByteArray?,
    ) {
        val ports = rejoinPorts(accountIdBytes, accountIdB64)
        var first = true
        while (currentCoroutineContextActive() && bound.isCurrent) {
            try {
                val seenHandleRevision = loginHandleRevision.value
                val outcome = rejoinOnce(ports, first)
                first = false
                if (outcome == RejoinPass.NO_LOGIN_HANDLE) {
                    // Another device of the account is listed: never found
                    // here. Look for the login handle again after a while,
                    // or at once when the account's username is learned.
                    Log.i(LOG_TAG, "DeviceGroup rejoin: no login handle to knock with; waiting, not founding")
                    awaitLoginHandleOrRetry(loginHandleRevision, seenHandleRevision)
                    continue
                }
                if (outcome == RejoinPass.JOINED) break
                if (outcome == RejoinPass.FOUND) {
                    // ADR-0081 third amendment, point 4 — no device answered:
                    // found the DeviceGroup here (a root reset of the chain the
                    // gone devices left) and start at once.
                    if (!bound.isCurrent || activeAccountIdB64.get() != accountIdB64) return
                    if (foundAfterSilence(accountIdBytes, accountIdB64, seedState, accountIdentitySeed)) return
                    delay(5000)
                }
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "DeviceGroup rejoin attempt failed; trying again (${rejoinFailureCode(cause)})")
                delay(5000)
            }
        }
        if (!bound.isCurrent || activeAccountIdB64.get() != accountIdB64) return
        rejoinJob = null
        Log.i(LOG_TAG, "rejoined the account's DeviceGroup")
        forgetCachedJournalSigners(accountIdB64)
        openAndPoll(accountIdBytes, accountIdB64, seedState = seedState, accountIdentitySeed = accountIdentitySeed)
    }

    /** A rejoined or newly founded DeviceGroup is a new epoch chain under the
     *  same journal id: drop the signers cached for the old one, so no read
     *  is signed with a key of a chain the DS no longer holds. */
    private fun forgetCachedJournalSigners(accountIdB64: String) {
        val groupId = DeviceGroupLifecycle.groupId(applicationContext, accountIdB64) ?: return
        DsSignerRegistry.shared.remove(dsGroupKey(groupId))
    }

    /** Found the DeviceGroup on this device after a knock nobody answered,
     *  and follow it. `false` when the founding failed (the caller knocks
     *  again). */
    private fun foundAfterSilence(
        accountIdBytes: ByteArray,
        accountIdB64: String,
        seedState: Boolean,
        accountIdentitySeed: ByteArray?,
    ): Boolean {
        val seed = try {
            me.lakko.app.auth.DeviceIdentityStore.ensureDeviceIdentitySeed(
                context = applicationContext,
                accountIdB64 = accountIdB64,
            )
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "founding after an unanswered knock: no device seed (${rejoinFailureCode(cause)})")
            return false
        }
        val ensured = try {
            DeviceGroupLifecycle.ensureDeviceGroup(
                context = applicationContext,
                accountIdBytes = accountIdBytes,
                accountIdB64 = accountIdB64,
                deviceIdentitySeed = seed,
                found = true,
            )
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "founding after an unanswered knock failed (${rejoinFailureCode(cause)})")
            return false
        } finally {
            seed.fill(0)
        }
        Log.i(LOG_TAG, "no device answered the knock; DeviceGroup founded")
        me.lakko.app.backup.BackupRestoreStore.recordRejoinAnswer(
            applicationContext, accountIdB64, me.lakko.app.backup.RestoreRejoinAnswer.FOUNDED,
        )
        // ADR-0090 decision A - this device is the account's only one: it
        // founds again the PublisherGroup of every channel the account owns.
        me.lakko.app.channels.ChannelRefounding.markFoundedAlone(applicationContext, accountIdB64)
        rejoinJob = null
        forgetCachedJournalSigners(accountIdB64)
        adopt(ensured, accountIdBytes, accountIdB64, seedState, accountIdentitySeed, 0L, false)
        return true
    }

    /**
     * Read a rejoin rendezvous until it holds an answer: a Welcome, or (with
     * [acceptPresence]) a presence note. Runs until cancelled; network
     * failures are retried.
     */
    private suspend fun readRendezvous(rendezvous: ByteArray, acceptPresence: Boolean): RendezvousAnswer {
        var cursor = 0L
        DsClient().use { ds ->
            while (true) {
                val result = try {
                    ds.fetchRendezvous(rendezvous, after = cursor, waitSecs = KNOCK_LISTEN_WAIT_SECS)
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (http: DsHttpError) {
                    if (http.status != 404) Log.w(LOG_TAG, "rejoin rendezvous read: http ${http.status}")
                    delay(250)
                    continue
                } catch (_: DsNetworkError) {
                    delay(250)
                    continue
                }
                if (result.messages.isEmpty()) continue
                cursor = result.messages.maxOf { it.id }
                val answer = rendezvousAnswer(result.messages.map { it.payload }) {
                    uniffi.lakko_ffi_uniffi.isRejoinPresent(it)
                }
                when {
                    answer is RendezvousAnswer.Welcome -> return answer
                    answer == RendezvousAnswer.Present && acceptPresence -> return answer
                    else -> continue
                }
            }
        }
        @Suppress("UNREACHABLE_CODE")
        error("unreachable")
    }

    private fun rejoinPorts(accountIdBytes: ByteArray, accountIdB64: String): RejoinPorts = object : RejoinPorts {
        private val b64e = java.util.Base64.getEncoder()
        private val b64d = java.util.Base64.getDecoder()

        override fun dropLocalState() {
            DeviceGroupSnapshotStore.dropStateForRejoin(applicationContext, accountIdB64)
        }

        override suspend fun publishDirectoryKeyPackage() {
            directoryKeyPackagePublisher?.invoke()
        }

        override fun loadState(): RejoinState? =
            DeviceGroupSnapshotStore.getPending(
                applicationContext, accountIdB64, DeviceGroupSnapshotStore.PendingKind.REJOIN,
            )?.let { RejoinState.decode(it) }

        override fun saveState(state: RejoinState) {
            DeviceGroupSnapshotStore.putPending(
                applicationContext, accountIdB64, DeviceGroupSnapshotStore.PendingKind.REJOIN, state.encoded(),
            )
        }

        override fun hasLoginHandle(): Boolean = knockLoginHandle(accountIdB64) != null

        override fun recordAnswered() {
            me.lakko.app.backup.BackupRestoreStore.recordRejoinAnswer(
                applicationContext, accountIdB64, me.lakko.app.backup.RestoreRejoinAnswer.ANSWERED,
            )
        }

        override suspend fun knockAndListen(nowMs: Long): KnockAnswer {
            val loginHandle = knockLoginHandle(accountIdB64)
                ?: throw RejoinUnavailable("no-login-handle")
            val groupId = DeviceGroupLifecycle.groupId(applicationContext, accountIdB64)
                ?: throw RejoinUnavailable("no-account-seed")
            // G-55: the knock is sealed under the seed the DeviceGroup id
            // derives from.
            val lineageSeed = DeviceGroupLifecycle.lineageSeed(applicationContext, accountIdB64)
                ?: throw RejoinUnavailable("no-account-seed")
            val seed = me.lakko.app.auth.DeviceIdentityStore.ensureDeviceIdentitySeed(
                context = applicationContext, accountIdB64 = accountIdB64,
            )
            val joiner = try {
                Identity.fromSeed(seed)
            } finally {
                seed.fill(0)
            }
            val rendezvous: ByteArray
            val payload: ByteArray
            val state: RejoinState
            try {
                val keyPackage = joiner.createKeyPackage()
                rendezvous = me.lakko.app.devices.PairingCodec.generateRendezvousPseudonym()
                // ADR-0081 amendment — the device identity's key IS this
                // device's old DeviceGroup leaf key: it signs that its leaf may
                // be replaced by this KeyPackage in the commit leaving the
                // journal's latest epoch (an empty knock is a probe that names
                // it), which lets a sibling readmit it under the
                // attested-removal policy without a passkey. An unclaimed
                // journal names no epoch: no proof.
                val probeEpoch = DsClient().use { ds -> ds.knock(groupId, emptyList()) }.epoch
                val proof = if (probeEpoch == null) {
                    ByteArray(0)
                } else {
                    uniffi.lakko_ffi_uniffi.signDeviceGroupSelfRemoval(joiner, groupId, probeEpoch, keyPackage)
                }
                // G-55: sealed under the lineage seed either way.
                payload = me.lakko.app.devices.PairingCodec.buildRejoinKnockPayload(
                    lineageSeed, loginHandle, keyPackage, rendezvous, proof,
                )
                val snapshot = joiner.snapshotStorage()
                state = try {
                    RejoinState(
                        rendezvousB64 = b64e.encodeToString(rendezvous),
                        joinerSnapshotB64 = b64e.encodeToString(snapshot),
                        knockedAtMs = nowMs,
                    )
                } finally {
                    snapshot.fill(0)
                }
            } finally {
                joiner.close()
                lineageSeed.fill(0)
            }
            // Kept BEFORE the knock: the joiner's provider is the only thing
            // that can open the Welcome a sibling sends.
            saveState(state)
            // ADR-0081 third amendment, point 3 — the rendezvous is read
            // before the knock goes out, the knock's own round trip is timed,
            // and the answer is waited for max(4 x rtt, 1 s).
            val answer = coroutineScope {
                val reader = async { readRendezvous(rendezvous, acceptPresence = true) }
                try {
                    val sent = System.nanoTime()
                    DsClient().use { ds -> ds.knock(groupId, listOf(payload)) }
                    val knocked = System.nanoTime()
                    val rttMs = (knocked - sent) / 1_000_000L
                    val windowMs = rejoinAnswerWindowMs(rttMs)
                    val got = withTimeoutOrNull(windowMs) { reader.await() }
                    if (got == null) {
                        Log.i(LOG_TAG, "rejoin knock: no answer in $windowMs ms (knock rtt $rttMs ms)")
                    } else {
                        val tookMs = (System.nanoTime() - knocked) / 1_000_000L
                        val what = if (got is RendezvousAnswer.Welcome) "a Welcome" else "a presence note"
                        Log.i(LOG_TAG, "rejoin knock: $what after $tookMs ms (knock rtt $rttMs ms, window $windowMs ms)")
                    }
                    got
                } finally {
                    reader.cancel()
                }
            }
            return KnockAnswer(state, answer)
        }

        override suspend fun awaitWelcome(state: RejoinState, deadlineMs: Long): ByteArray? {
            val rendezvous = b64d.decode(state.rendezvousB64)
            val remaining = (deadlineMs - System.currentTimeMillis()).coerceIn(1L, REKNOCK_AFTER_MS)
            val got = withTimeoutOrNull(remaining) { readRendezvous(rendezvous, acceptPresence = false) }
            return (got as? RendezvousAnswer.Welcome)?.bytes
        }

        override suspend fun join(state: RejoinState, welcome: ByteArray) {
            val seed = me.lakko.app.auth.DeviceIdentityStore.ensureDeviceIdentitySeed(
                context = applicationContext, accountIdB64 = accountIdB64,
            )
            val joiner = try {
                Identity.fromSeed(seed)
            } finally {
                seed.fill(0)
            }
            try {
                val snapshot = b64d.decode(state.joinerSnapshotB64)
                try {
                    joiner.restoreStorage(snapshot)
                } finally {
                    snapshot.fill(0)
                }
                DeviceGroup.joinFromWelcome(welcome, joiner, accountIdBytes).close()
                DeviceGroupLifecycle.persistSnapshotOrThrow(applicationContext, joiner, accountIdB64)
                // Joined, not founded: a founding mark left from an earlier
                // founding on this device must not turn the next claim-on-open
                // into a root reset of the chain this device just joined.
                DeviceGroupJournalAccess.clearFoundingUnconfirmed(applicationContext, accountIdB64)
                // A fresh observer of the journal.
                DeviceGroupSnapshotStore.putPending(
                    applicationContext, accountIdB64, DeviceGroupSnapshotStore.PendingKind.ADMISSIONS,
                    AdmissionRecord(watchingSinceMs = System.currentTimeMillis()).encoded(),
                )
            } finally {
                joiner.close()
            }
        }

        override fun clearState() {
            DeviceGroupSnapshotStore.deletePending(applicationContext, accountIdB64, DeviceGroupSnapshotStore.PendingKind.REJOIN)
        }

        override fun now(): Long = System.currentTimeMillis()
    }

    /** The directory check of the founding policy: `true` when the account's
     *  directory lists no own device other than this one. A failure throws
     *  (nothing is founded on an unread directory). */
    private suspend fun directoryListsNoOtherDevice(accountIdBytes: ByteArray, deviceSeed: ByteArray): Boolean {
        val ownKey = Identity.fromSeed(deviceSeed).use { it.signaturePublicKey() }
        val listed = me.lakko.app.discovery.DiscoveryClient().use { it.fetchKeyPackages(accountIdBytes) }.orEmpty()
        return listed.all { it.deviceId.contentEquals(ownKey) }
    }

    /** The account's directory rows whose device binding verifies under the
     *  account key the conversation leaves are bound to: leaf key hex to its
     *  directory KeyPackage. A deleted device is tombstoned (ADR-0074) and
     *  not listed. */
    private suspend fun ownDirectory(accountIdBytes: ByteArray, accountIdB64: String): Map<String, ByteArray> {
        val lineage = DeviceGroupLifecycle.lineageSeed(applicationContext, accountIdB64) ?: return emptyMap()
        val accountKey = try {
            Identity.fromSeed(lineage).use { it.signaturePublicKey() }
        } finally {
            lineage.fill(0)
        }
        val listed = me.lakko.app.discovery.DiscoveryClient().use { it.fetchKeyPackages(accountIdBytes) }.orEmpty()
        val out = HashMap<String, ByteArray>()
        for (row in listed) {
            val bound = runCatching { uniffi.lakko_ffi_uniffi.keyPackageAccountKey(row.keyPackage) }.getOrNull()
                ?: continue
            // A Phase 1 unbound leaf resolves to itself: not a binding.
            if (!bound.contentEquals(accountKey) || bound.contentEquals(row.deviceId)) continue
            out[hexOf(row.deviceId)] = row.keyPackage
        }
        return out
    }

    private fun loadAdmissions(accountIdB64: String) {
        admissions = DeviceGroupSnapshotStore.getPending(
            applicationContext, accountIdB64, DeviceGroupSnapshotStore.PendingKind.ADMISSIONS,
        )?.let { AdmissionRecord.decode(it) } ?: AdmissionRecord(watchingSinceMs = System.currentTimeMillis())
    }

    private fun saveAdmissions(accountIdB64: String) {
        val record = admissions ?: return
        if (activeAccountIdB64.get() != accountIdB64) return
        try {
            DeviceGroupSnapshotStore.putPending(
                applicationContext, accountIdB64, DeviceGroupSnapshotStore.PendingKind.ADMISSIONS, record.encoded(),
            )
        } catch (cause: Throwable) {
            // The record is a guard against admitting twice; an unsaved update
            // is rebuilt from the journal by the next batch.
            Log.w(LOG_TAG, "DeviceGroup admission record not saved (${cause.javaClass.simpleName})")
        }
    }

    /** Record every key a commit added, at the row's DS time. */
    private fun noteAdditions(before: List<ByteArray>?, g: DeviceGroup, createdAt: String) {
        val record = admissions ?: return
        before ?: return
        val after = runCatching { g.devices().map { it.signaturePublicKey } }.getOrNull() ?: return
        val beforeHex = before.map { hexOf(it) }.toSet()
        val at = parseMs(createdAt) ?: System.currentTimeMillis()
        admissions = after.map { hexOf(it) }.filter { it !in beforeHex }
            .fold(record) { acc, keyHex -> acc.noteAdded(keyHex, at) }
    }

    private fun parseMs(iso: String): Long? =
        try {
            java.time.OffsetDateTime.parse(iso).toInstant().toEpochMilli()
        } catch (_: Throwable) {
            try {
                java.time.Instant.parse(iso).toEpochMilli()
            } catch (_: Throwable) {
                null
            }
        }

    /**
     * ADR-0081 third amendment, points 1 and 2 — while this device shows, a
     * long poll on its DeviceGroup's knocks. A knock naming this account's
     * login handle gets a presence note on its rendezvous at once, before any
     * directory lookup, so the knocking device learns in one round trip that
     * a device is running; the admission check then runs at the poll's next
     * pass. In the background (or not running) this device does not answer.
     */
    private suspend fun runKnockListener(bound: me.lakko.app.Binding, accountIdB64: String, groupId: ByteArray) {
        var cursor = 0L
        var firstRead = true
        val answered = HashSet<Long>()
        DsClient().use { ds ->
            while (currentCoroutineContextActive() && bound.isCurrent) {
                appForeground.first { it }
                val loginHandle = knockLoginHandle(accountIdB64)
                if (loginHandle == null) {
                    delay(5000)
                    continue
                }
                val result = try {
                    coroutineScope {
                        val wentAway = async { appForeground.first { !it } }
                        val fetch = async {
                            ds.fetchKnocks(
                                groupId,
                                after = cursor,
                                waitSecs = if (firstRead) 0 else KNOCK_LISTEN_WAIT_SECS,
                            )
                        }
                        val got = select<me.lakko.app.ds.FetchResult?> {
                            fetch.onAwait { it }
                            wentAway.onAwait { null }
                        }
                        wentAway.cancel()
                        fetch.cancel()
                        got
                    }
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (cause: Throwable) {
                    Log.w(LOG_TAG, "DeviceGroup knock listen failed (${rejoinFailureCode(cause)})")
                    delay(5000)
                    continue
                } ?: continue
                if (!bound.isCurrent) return
                val backlog = firstRead
                firstRead = false
                if (result.messages.isEmpty()) continue
                cursor = result.messages.maxOf { it.id }
                val ownKey = runCatching { identity.get()?.signaturePublicKey() }.getOrNull()
                val nowMs = System.currentTimeMillis()
                var forThisAccount = false
                // G-55: knocks are sealed under the seed the DeviceGroup id
                // derives from.
                val lineageSeed = DeviceGroupLifecycle.lineageSeed(applicationContext, accountIdB64) ?: continue
                for (knock in result.messages) {
                    if (!answered.add(knock.id)) continue
                    val decoded = try {
                        me.lakko.app.devices.PairingCodec.openRejoinKnockPayload(lineageSeed, knock.payload, loginHandle)
                    } catch (_: Throwable) {
                        continue // not a sealed knock of this account
                    }
                    forThisAccount = true
                    val knockingKey = runCatching {
                        uniffi.lakko_ffi_uniffi.keyPackageSignatureKey(decoded.keyPackage)
                    }.getOrNull()
                    if (knockingKey != null && ownKey != null && knockingKey.contentEquals(ownKey)) continue
                    if (backlog && !shouldAnswerBacklogKnock(parseMs(knock.createdAt), nowMs)) continue
                    try {
                        DsClient().use {
                            it.publishRendezvous(decoded.rendezvousPseudonym, uniffi.lakko_ffi_uniffi.composeRejoinPresent())
                        }
                        Log.i(LOG_TAG, "answered a knock with a presence note")
                    } catch (cancellation: CancellationException) {
                        throw cancellation
                    } catch (cause: Throwable) {
                        Log.w(LOG_TAG, "presence note not posted (${rejoinFailureCode(cause)})")
                    }
                }
                if (answered.size > MAX_HANDLED_KNOCKS) {
                    val keep = answered.sorted().takeLast(MAX_HANDLED_KNOCKS)
                    answered.clear()
                    answered.addAll(keep)
                }
                if (forThisAccount) {
                    knocksPending.set(true)
                    knockFetchNow.set(true)
                    headKick.trySend(Unit)
                }
            }
        }
    }

    /**
     * This account's login handle for a knock: the session's, else the one
     * the account's own-username register names (a passkey restore lands
     * with no username in the session, and the register the backup restored
     * goes live only once this runtime runs).
     */
    /** The register's name in the live synced state, or `null`. */
    private fun liveOwnUsername(): String? =
        synchronized(stateLock) { runCatching { syncedState?.ownUsername() }.getOrNull() }
            ?.trim()?.takeIf { it.isNotEmpty() }

    private fun knockLoginHandle(accountIdB64: String): ByteArray? {
        loginHandleProvider?.invoke()?.let { return it }
        val name = synchronized(stateLock) { runCatching { syncedState?.ownUsername() }.getOrNull() }
            ?: runCatching {
                DeviceGroupSnapshotStore.getSnapshot(
                    context = applicationContext,
                    accountIdB64 = accountIdB64,
                    dir = DeviceGroupSnapshotStore.SYNC_STATE_DIR,
                )?.let { bytes -> SyncedState.fromBytes(bytes).use { it.ownUsername() } }
            }.getOrNull()
        val trimmed = name?.trim()?.takeIf { it.isNotEmpty() } ?: return null
        return runCatching { uniffi.lakko_ffi_uniffi.loginHandle(trimmed) }.getOrNull()
    }

    /**
     * A sibling's side: read the DeviceGroup's knocks and admit each one
     * [admissionVerdict] allows — replace the device's previous leaf, if it
     * is still in the roster, by its KeyPackage in one commit carrying its
     * self-removal proof (ADR-0081 amendment), else add the KeyPackage; one
     * commit through the outbox, the Welcome to the knock's rendezvous.
     */
    private suspend fun checkKnocks(accountIdBytes: ByteArray, accountIdB64: String, bound: me.lakko.app.Binding) {
        val g = group.get() ?: return
        val id = identity.get() ?: return
        if (admissions == null) return
        val loginHandle = knockLoginHandle(accountIdB64) ?: return
        // G-55: knocks are sealed under the seed the DeviceGroup id derives from.
        val lineageSeed = DeviceGroupLifecycle.lineageSeed(applicationContext, accountIdB64) ?: return
        val knocks = DsClient().use { ds -> ds.fetchKnocks(g.groupId(), after = knockCursor) }
        if (!bound.isCurrent || knocks.messages.isEmpty()) return
        // A page that was full: the next check reads on.
        if (knocks.hasMore) knocksPending.set(true)
        val roster = runCatching { g.devices() }.getOrNull()?.map {
            RosterMember(it.leafIndex, hexOf(it.signaturePublicKey))
        } ?: return
        val ownKeyHex = hexOf(id.signaturePublicKey())
        val policy = runCatching { g.requireAttestedRemovals() }.getOrDefault(false)
        val heldGroupId = g.groupId()
        val heldEpoch = g.epoch()
        var directory: Map<String, ByteArray>? = null
        for (knock in knocks.messages) {
            if (!bound.isCurrent) return
            // Advanced knock by knock: an admission returns early (its commit
            // reopens this runtime), and the knocks after it are read again.
            knockCursor = maxOf(knockCursor, knock.id)
            val decoded = try {
                me.lakko.app.devices.PairingCodec.openRejoinKnockPayload(lineageSeed, knock.payload, loginHandle)
            } catch (_: Throwable) {
                continue // not a sealed knock of this account (or not a pairing payload)
            }
            val keyBytes = runCatching { uniffi.lakko_ffi_uniffi.keyPackageSignatureKey(decoded.keyPackage) }
                .getOrNull() ?: continue
            val keyHex = hexOf(keyBytes)
            // Checked BEFORE a commit is built: a commit the DS takes but every
            // peer refuses would leave this device ahead of them.
            val proof = decoded.selfRemovalProof
            val proofCovers = proof != null && uniffi.lakko_ffi_uniffi.deviceGroupSelfRemovalCovers(
                id, proof, heldGroupId, heldEpoch, keyBytes, decoded.keyPackage,
            )
            val dir = directory ?: ownDirectory(accountIdBytes, accountIdB64).also { directory = it }
            if (!bound.isCurrent) return
            val record = admissions ?: return
            val nowMs = System.currentTimeMillis()
            val verdict = admissionVerdict(
                AdmissionInput(
                    knockId = knock.id,
                    knockAtMs = parseMs(knock.createdAt) ?: 0L,
                    keyHex = keyHex,
                    ownKeyHex = ownKeyHex,
                    roster = roster,
                    handled = record.handled.toSet(),
                    watchingSinceMs = record.watchingSinceMs,
                    lastAddedAtMs = record.lastAddedAtMs[keyHex],
                    listed = dir.containsKey(keyHex),
                    policyRequiresAttestation = policy,
                    nowMs = nowMs,
                    selfRemovalProofCovers = proofCovers,
                    keyPackageAlreadyIn = runCatching {
                        g.containsLeafOfKeyPackage(id, decoded.keyPackage)
                    }.getOrDefault(false),
                ),
            )
            if (verdict != AdmissionVerdict.ADMIT) continue
            val added = me.lakko.app.devices.DeviceGroupCommits.commitChange(
                context = applicationContext,
                accountIdBytes = accountIdBytes,
                accountIdB64 = accountIdB64,
                build = { fresh ->
                    val before = runCatching { fresh.group.devices() }.getOrNull().orEmpty()
                    val previous = before.firstOrNull { hexOf(it.signaturePublicKey) == keyHex }
                    val answered = runCatching {
                        fresh.group.containsLeafOfKeyPackage(fresh.identity, decoded.keyPackage)
                    }.getOrDefault(false)
                    when {
                        // Another sibling answered it meanwhile.
                        answered -> null
                        previous == null -> {
                            val addition = fresh.group.addDeviceFromWire(fresh.identity, decoded.keyPackage)
                            me.lakko.app.devices.DeviceGroupCommits.Built(
                                kind = me.lakko.app.devices.DeviceGroupCommitEntry.Kind.REJOIN_ADD,
                                row = DeviceGroupJournalEnvelope.encode(addition.commit, null),
                                welcome = addition.welcome,
                                welcomeRendezvous = decoded.rendezvousPseudonym,
                                priorLeaves = before.map { it.leafIndex },
                            )
                        }
                        else -> {
                            // ADR-0081 amendment — the previous leaf out and the
                            // new one in, in ONE commit; the proof rides in the
                            // removal-authorisation slot. Re-checked at the
                            // epoch this commit leaves: a proof names one
                            // epoch, and peers refuse it at any other. Under
                            // the policy a removal it does not cover would be
                            // refused by every peer.
                            val covers = proof != null && uniffi.lakko_ffi_uniffi.deviceGroupSelfRemovalCovers(
                                fresh.identity, proof, fresh.group.groupId(), fresh.group.epoch(),
                                keyBytes, decoded.keyPackage,
                            )
                            val policyNow = runCatching { fresh.group.requireAttestedRemovals() }.getOrDefault(true)
                            if (!covers && policyNow) {
                                null
                            } else {
                                val replaced = fresh.group.replaceDeviceAt(
                                    fresh.identity, previous.leafIndex, decoded.keyPackage,
                                )
                                me.lakko.app.devices.DeviceGroupCommits.Built(
                                    kind = me.lakko.app.devices.DeviceGroupCommitEntry.Kind.REJOIN_REPLACE,
                                    row = DeviceGroupJournalEnvelope.encode(
                                        replaced.commit, if (covers) proof else null,
                                    ),
                                    welcome = replaced.welcome,
                                    welcomeRendezvous = decoded.rendezvousPseudonym,
                                    priorLeaves = before.map { it.leafIndex },
                                )
                            }
                        }
                    }
                },
                after = { },
            )
            if (added?.outcome is me.lakko.app.devices.DeviceGroupCommitOutcome.Landed) {
                admissions = admissions?.noteAdded(keyHex, nowMs)?.noteHandled(knock.id)
                saveAdmissions(accountIdB64)
                Log.i(LOG_TAG, "a device of this account was admitted again")
            } else {
                // Another running device took the epoch (409), or the commit
                // was not needed: nothing of this one is on the DS.
                Log.i(LOG_TAG, "a knock was not admitted here (another device took the epoch, or nothing to do)")
            }
            // The commits reopened this runtime; the next pass re-reads.
            if (knocks.messages.last().id > knock.id) knocksPending.set(true)
            return
        }
    }

    /**
     * ADR-0060 section 7 — send one `lakko-contacts` envelope (Visibility or
     * ReachState, stamped by the reach manager and never re-stamped here) to
     * the account's other devices as a DeviceGroup `ServiceEnvelope` body.
     * The snapshot is persisted before the publish, so a crash in between
     * never lets a later handle reuse this message's ratchet position.
     */
    private suspend fun sendServiceEnvelope(envelope: ByteArray) {
        onHandleCaughtUp {
            check(!mutableDeviceRemoved.value) { "this device was removed from the account" }
            val g = group.get() ?: error("DeviceGroupSync not bootstrapped")
            val id = identity.get() ?: error("DeviceGroupSync not bootstrapped")
            val accountIdB64 = activeAccountIdB64.get()
                ?: error("DeviceGroupSync not bootstrapped")
            val wireBytes = g.createServiceEnvelopeMessage(id, envelope, Hlc.mint())
            persistHandleOrThrow(id, accountIdB64)
            DsClient().use { ds ->
                ds.publishGroup(g.groupId(), DeviceGroupJournalEnvelope.encode(wireBytes, null))
            }
        }
    }

    // ------------------------------------------------------------
    // ADR-0074 part 3 - the key-rotation envelope
    // ------------------------------------------------------------

    /** Bumped each time a poll read the journal to its head. */
    private val headReads: MutableStateFlow<Long> = MutableStateFlow(0L)

    /** Callers waiting in [awaitHead]. While any wait, the poll does not hold
     *  its read open: two head reads of a 25 s long poll each made every
     *  step of an invite-link answer take about 50 s. */
    private val headWaiters = java.util.concurrent.atomic.AtomicInteger(0)

    /** Cuts short a long poll already out when a caller starts waiting. */
    private val headKick = Channel<Unit>(Channel.CONFLATED)

    /** One journal read: at once while a head waiter exists, else a long poll
     *  that a new waiter cuts short (`null`: read again at once). */
    private suspend fun fetchOrKicked(ds: DsClient, groupId: ByteArray, after: Long) =
        if (headWaiters.get() > 0 || knockFetchNow.getAndSet(false)) {
            ds.fetchGroup(groupId = groupId, after = after, limit = null, waitSecs = 0)
        } else {
            coroutineScope {
                val fetch = async { ds.fetchGroup(groupId = groupId, after = after, limit = null, waitSecs = 25) }
                val kicked = select {
                    fetch.onAwait { false }
                    headKick.onReceive { true }
                }
                if (kicked) {
                    fetch.cancel()
                    null
                } else {
                    fetch.await()
                }
            }
        }

    private fun isConversationReentry(envelope: ByteArray): Boolean =
        try {
            uniffi.lakko_ffi_uniffi.dispatchIncomingEnvelope(envelope).kind ==
                uniffi.lakko_ffi_uniffi.FfiIncomingEnvelopeKind.CONVERSATION_REENTRY
        } catch (_: Throwable) {
            false
        }

    /**
     * ADR-0091 phase 2 route 1 — send a conversation re-entry request to
     * this account's other devices as a DeviceGroup `ServiceEnvelope`. Throws
     * when it was not sent.
     */
    public suspend fun sendConversationReentry(envelope: ByteArray) {
        sendServiceEnvelope(envelope)
    }

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
        headWaiters.incrementAndGet()
        headKick.trySend(Unit)
        try {
            return kotlinx.coroutines.withTimeoutOrNull(timeoutMs) {
                // Two head reads: the first may belong to a fetch that was
                // already out when this was asked.
                headReads.first { it >= seen + 2 }
                true
            } ?: (headReads.value > seen)
        } finally {
            headWaiters.decrementAndGet()
        }
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
        val seal = {
            check(!mutableDeviceRemoved.value) { "this device was removed from the account" }
            val g = group.get() ?: error("DeviceGroupSync not bootstrapped")
            val id = identity.get() ?: error("DeviceGroupSync not bootstrapped")
            val accountIdB64 = activeAccountIdB64.get() ?: error("DeviceGroupSync not bootstrapped")
            val wire = g.createServiceEnvelopeMessage(id, envelope, Hlc.mint())
            persistHandleOrThrow(id, accountIdB64)
            val sealedAt = uniffi.lakko_ffi_uniffi.epochOfMessage(wire)
                ?: error("the sealed message carries no epoch")
            wire to sealedAt
        }
        return try {
            seal()
        } catch (_: DeviceGroupSnapshotStore.SnapshotChanged) {
            reopenBehindDisk()
            seal()
        }
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
        rejoinJob?.cancel()
        rejoinJob = null
        identity.set(null)
        group.set(null)
        handleGeneration = null
        admissions = null
        knocksPending.set(false)
        knockFetchNow.set(false)
        knockCursor = 0L
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

        /** How long a link-answer verdict waits for the fold (ADR-0079). Chosen,
         *  not measured. */
        /** How long a claim waits for the journal to be read past it: one
         *  long poll (25 s) twice over and a margin. Chosen, not measured. */
        internal const val CLAIM_HEAD_TIMEOUT_MS: Long = 60_000L

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

        /** ADR-0091 phase 2 route 1 — a conversation re-entry request from
         *  another device of this account (the envelope, and the sender's
         *  DeviceGroup leaf key resolved at receipt). Set by the chat layer;
         *  `null` drops it. Never rendered. */
        @Volatile
        public var conversationReentrySink: ((envelope: ByteArray, senderLeafKey: ByteArray?) -> Unit)? = null

        /** ADR-0081 decision 4 — this account's login handle (the knock
         *  payload names it; a sibling admits only a knock naming it). Set by
         *  the app root; `null` for a guest, who has no other device. */
        @Volatile
        public var loginHandleProvider: (() -> ByteArray?)? = null

        /** ADR-0081 third amendment — whether the app is showing. A device
         *  answers knocks only then (set from `MainActivity`). */
        private val appForeground: MutableStateFlow<Boolean> = MutableStateFlow(false)

        /** The app came to the foreground ([shown]) or left it. */
        public fun setAppForeground(shown: Boolean) {
            appForeground.value = shown
        }

        /** ADR-0081 decision 4 — publish this device's directory KeyPackage
         *  (a rejoining device's first step: its signed device binding is
         *  what a sibling checks). Set by the app root. */
        @Volatile
        public var directoryKeyPackagePublisher: (suspend () -> Unit)? = null

        /**
         * Wait until the live runtime read its journal to the head, once.
         * The runtime may not be up yet right after a launch: waits for it
         * within the same [timeoutMs]. `false` when the head was not seen.
         */
        /** Whether a sync runtime is polling a DeviceGroup right now. */
        public fun hasLiveRuntime(): Boolean =
            active?.let { it.group.get() != null && it.pollJob?.isActive == true } == true

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

        /** Moves when this account's username is learned on this device, so a
         *  rejoin waiting for a login handle knocks at once. */
        private val loginHandleRevision: MutableStateFlow<Long> = MutableStateFlow(0L)

        /** The account's username was learned (a passkey sign-in's prompt, or
         *  the backup's name confirmed by auth): wake a waiting rejoin. */
        public fun wakeForLoginHandle() {
            loginHandleRevision.value = loginHandleRevision.value + 1
        }

        /**
         * The own-username register as the stored synced state holds it (a
         * backup restore seeds that file before the sync runtime runs), or
         * `null`. Not checked here: `auth/UsernamePrompt.kt` asks auth first.
         */
        public fun storedOwnUsername(context: Context, accountIdB64: String): String? {
            val live = active
            if (live != null && live.activeAccountIdB64.get() == accountIdB64) {
                live.liveOwnUsername()?.let { return it }
            }
            return runCatching {
                DeviceGroupSnapshotStore.getSnapshot(
                    context = context,
                    accountIdB64 = accountIdB64,
                    dir = DeviceGroupSnapshotStore.SYNC_STATE_DIR,
                )?.let { bytes -> SyncedState.fromBytes(bytes).use { it.ownUsername() } }
            }.getOrNull()?.trim()?.takeIf { it.isNotEmpty() }
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
                restored.quickReactionEnvelope()?.let { state.applyQuickReactionEnvelope(it) }
                // UX 17.6 - and the device acknowledgements.
                for (envelope in restored.deviceAckEnvelopes()) state.applyDeviceAckEnvelope(envelope)
                // ADR-0090 - and the channel subscriptions and admission
                // seeds, so a restore onto a device that kept its synced
                // state still learns the account's channels.
                for (envelope in restored.channelSubscriptionEnvelopes()) state.applyChannelSubscriptionEnvelope(envelope)
                for (envelope in restored.channelAdmissionSeedEnvelopes()) state.applyChannelAdmissionSeedEnvelope(envelope)
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

/** A rejoin that cannot be made on this device right now, with a fixed code
 *  (never an identifier). */
internal class RejoinUnavailable(val code: String) : IllegalStateException("rejoin unavailable: $code")

/**
 * What a rejoin or knock failure was, for a log line: the class and, for the
 * DS, the status and its error code. Never the message (a DS error's names
 * the journal's id) and never an identifier.
 */
internal fun rejoinFailureCode(cause: Throwable): String = when (cause) {
    is RejoinUnavailable -> "unavailable ${cause.code}"
    is me.lakko.app.ds.DsProofRefusedError ->
        if (cause.clockSkewed) "proof refused, device clock off" else "proof refused"
    is DsHttpError -> listOfNotNull("http ${cause.status}", dsErrorCode(cause.body)).joinToString(" ")
    is DsNetworkError -> "network ${cause.cause?.javaClass?.simpleName ?: "error"}"
    else -> cause.javaClass.simpleName.ifEmpty { "error" }
}
