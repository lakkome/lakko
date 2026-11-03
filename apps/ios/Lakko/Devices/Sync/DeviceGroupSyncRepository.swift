// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// Cross-device sync runtime on iOS (Phase 2a3e2b).
///
/// Mirror of `apps/web/src/devices/sync/DeviceGroupSyncContext.tsx`
/// translated into a `@MainActor` `ObservableObject`. Owns the
/// per-session DeviceGroup handle + the four reactive state
/// maps (drafts / flags / readCursors / order) that the UI
/// observes via `@Published`, plus a background poll loop that
/// drains the DeviceGroup DS journal and a set of typed send
/// paths for the outbound side.
///
/// Lifecycle:
///
///   * [`bootstrap`] (idempotent for the same `accountId`) —
///     loads the DeviceGroup handle from the existing snapshot and kicks
///     off the poll loop. With no usable snapshot (ADR-0081 decision 4) it
///     founds only when the account's directory lists no other own device,
///     and otherwise REJOINS through a sibling (``DeviceGroupRejoin``).
///   * [`clear`] — cancels the poll loop, drops the handle,
///     clears every reactive map. Called from
///     ``AuthRepository/logout`` and from
///     ``AppServices/wipeAccountLocalData(accountIdB64:)``, which then
///     removes the persisted state (``AccountLocalDataWipe``).
///
/// The poll loop runs in a single `Task` cancelled on `clear`;
/// the send paths await on the main actor.
///
/// ADR-0058: the maps are no longer folded here. One `SyncedState` (the
/// `lakko-sync` fold over the FFI) takes every received payload and every
/// local write, is persisted as its own bytes, and the published maps plus
/// the chat folders are projections of it. A local write is folded first and
/// published after; a publish that fails sets a persisted pending flag, and the
/// next successful poll fetch re-sends the whole state (``PendingSyncTracker``).
/// When the DeviceGroup grows, the lowest prior leaf re-sends the whole state
/// so a new device starts with it.
@MainActor
public final class DeviceGroupSyncRepository: ObservableObject {

    // MARK: - Public reactive state

    /// `true` once the per-session DeviceGroup handle is loaded
    /// and the poll loop has started.
    @Published public private(set) var ready: Bool = false

    /// Per-group draft (composer text) snapshot. Key is the
    /// `groupIdHex(groupId)` value the receiver-side helper
    /// produces. A projection of ``syncedState``, rebuilt after every fold.
    @Published public private(set) var drafts: [String: DraftState] = [:]

    /// Per-group UI flags snapshot (pin / mute / archive /
    /// unread-override).
    @Published public private(set) var flags: [String: FlagsState] = [:] {
        didSet {
            if flags != oldValue { mirrorMutedConversations() }
        }
    }

    /// Per-group read cursor snapshot.
    @Published public private(set) var readCursors: [String: ReadCursorState] = [:]

    /// Whole-account explicit conversation order. `nil` when no
    /// `Order` payload has arrived yet.
    @Published public private(set) var order: OrderState?

    /// ADR-0058 — the person's chat folders, in bar order. Only folders
    /// whose name has arrived, never a deleted one (the fold decides both).
    @Published public private(set) var folders: [FolderView] = []
    /// ADR-0065 section 4 — the account's saved messages (live entries,
    /// most recently saved first).
    @Published public private(set) var saved: [SavedMessageView] = []

    /// ADR-0073 R.5 — the conversation lineage registers: which group
    /// continues which conversation. Read through ``ConversationLineage``.
    @Published public private(set) var lineage: [LineageEntry] = []

    /// ADR-0087 section 9 — the conversations this account's membership of
    /// ended (the fold's reading: a later `joined` takes one off the list).
    @Published public private(set) var endedConversations: [ConversationEndRegister] = []

    /// ADR-0075 (amended 2026-10-13) — the account's recovery email address,
    /// as this account's devices keep it: one register of the synced state,
    /// so it is on disk under the device key, with the sibling devices and in
    /// the phrase-sealed backup. `nil` when no device ever wrote one, or it
    /// was removed. Never logged.
    @Published public private(set) var recoveryEmail: String?

    /// ADR-0076 section 3 — the account's own username, as its devices keep
    /// it: one more register of the synced state. `nil` when never written.
    @Published public private(set) var ownUsername: String?
    /// ADR-0079 section 4 — the account's own invite links, every device's,
    /// revoked ones included.
    @Published public private(set) var linkRecords: [LinkRecord] = []
    /// ADR-0079 (reworked 2026-10-17) — bumped whenever a claim of a link
    /// request from another device of the account is recorded.
    @Published public private(set) var linkClaimsVersion: Int = 0
    /// When the last fetch that found the journal drained had STARTED (unix
    /// ms): a fetch that started after a claim was published read every row
    /// up to it.
    private var lastDrainedFetchStartedAtMs: Int64 = 0

    /// The synced state of the bound account was loaded from disk: a write
    /// that waited for it can run. Wired by the composition root.
    var onSyncedStateReady: (@MainActor () -> Void)?

    /// UX spec chat-folders section 6 — something folded here is still
    /// waiting to reach the other devices (this device is offline).
    @Published public private(set) var pendingSync: Bool = false

    /// Another device removed this one from the account, and this device has
    /// applied that removal (its DeviceGroup is inactive). The poll loop has
    /// stopped for good; the app root shows "This device was removed"
    /// (docs/ux/second-device.md section 3.1). Reset on sign-out and on an
    /// account switch.
    @Published public private(set) var deviceRemoved: Bool = false
    /// Whether another device of this account is signed in, as far as this
    /// one knows: the DeviceGroup roster, or the directory while this device
    /// holds no DeviceGroup. `nil` until it is known.
    @Published public private(set) var otherDeviceSignedIn: Bool?

    // MARK: - Internals

    private var identity: Identity?
    private var group: DeviceGroup?
    private var pollTask: Task<Void, Never>?
    /// Advanced whenever the runtime lets go of its account
    /// (``cancelPollAndDropHandles()``: ``clear()`` and an account switch in
    /// ``bootstrap(secrets:)``). A poll loop captures it when it starts: a
    /// cancelled loop still runs what follows an `await` that returned, and
    /// ``identity`` / ``group`` are by then the next account's.
    private let binding = BindingEpoch()
    /// The pending debounced snapshot write, cancelled by the next change.
    private var persistTask: Task<Void, Never>?
    private var activeAccountIdB64: String?
    /// ADR-0058 — the one fold. Every received payload and every local write
    /// goes through it; the published maps above are read back from it.
    /// Persisted by ``SyncedStateStore`` (its own folder under Application
    /// Support, sealed with the same Keychain wrap key the DeviceGroup
    /// snapshot uses).
    private var syncedState = SyncedState()
    /// Whether a publish failed and the state still has to be re-sent.
    /// Persisted per account; `nil` before bootstrap.
    private var pending: PendingSyncTracker?
    /// The pending growth re-send, so two growths in a row send once.
    private var resendTask: Task<Void, Never>?
    private var restoreObserver: NSObjectProtocol?
    /// The raw account id of the active session (the DeviceGroup commit
    /// outbox, the directory reads of a rejoin).
    private var activeAccountId: Data?
    /// ADR-0081 decision 3 — the snapshot generation the held handle was read
    /// at (or last wrote). Every write of the handle names it, so a handle
    /// still at N never puts N back over an N+1 another handle made current.
    private var handleGeneration: Int?
    /// ADR-0081 decision 4 — the founding decision or rejoin in progress.
    private var rejoinTask: Task<Void, Never>?
    /// ADR-0081 decision 4 — the sibling's admission record (when it started
    /// watching, which keys it saw added, which knocks it handled).
    private var admissions: DeviceGroupAdmissionRecord?
    /// When the knocks were last read, and the DS id of the last knock row
    /// read this session.
    private var lastKnockCheckAtMs: Int64 = 0
    private var knockCursor: Int64 = 0
    private var changedHereObserver: NSObjectProtocol?

    /// ADR-0081 decision 4 — this account's login handle (the knock payload
    /// names it; a sibling admits only a knock naming it). Wired by the
    /// composition root; `nil` for a guest, who has no other device.
    var loginHandleProvider: @MainActor () -> Data? = { nil }
    /// ADR-0081 decision 4 — publish this device's directory KeyPackage (a
    /// rejoining device's first step: its signed device binding is what a
    /// sibling checks). Wired by the composition root.
    var republishDirectoryKeyPackage: @MainActor () async throws -> Void = {}
    /// ADR-0060 section 7 — service envelopes whose send failed or came
    /// before the handle was ready, one per envelope kind (a newer one of the
    /// same kind replaces an older). Retried by the poll loop. In memory: a
    /// relaunch loses them, and the setting then reaches the other devices
    /// with its next change.
    private var pendingServiceEnvelopes: [String: Data] = [:]
    /// Where the bytes a received `ServiceEnvelope` carries go. The default
    /// hands them to ``ReachCenter``, which keeps a strictly newer Visibility,
    /// merges a ReachState, passes this account's own profile to
    /// ``ProfileDirectory`` (ADR-0048 section 4a), and drops everything else.
    var onServiceEnvelope: @MainActor (Data) -> Void = { envelope in
        let outcome = ReachCenter.shared.adoptDeviceEnvelope(envelope)
        if outcome == .foreign {
            NSLog("ADR-0060: a DeviceGroup service envelope of another kind was dropped")
        }
    }

    /// ADR-0074 section 3.3 — where a `key-rotation` envelope received in
    /// THIS account's DeviceGroup goes (the kept device's pending store, via
    /// ``AuthRepository/receiveKeyRotationEnvelope(_:arrival:)``). It carries
    /// the account's next seed, so it is handed over as bytes and never
    /// logged, rendered, or passed to ``onServiceEnvelope``. The default
    /// drops it.
    var onKeyRotationEnvelope: @MainActor (Data) -> Void = { _ in
        NSLog("ADR-0074: a key-rotation envelope arrived with no receiver wired; dropped")
    }
    /// UX second-device.md section 3 — an open Devices screen hears of a
    /// received commit once its snapshot is on disk.
    private var rosterChange = RosterChangeAfterPersist()
    /// When the poll loop last completed a fetch that reached the journal's
    /// head (unix ms); 0 before the first one. Read by
    /// ``waitForJournalHead(timeoutMs:)``.
    private var lastDrainedFetchAtMs: Int64 = 0
    /// Callers waiting in ``waitForJournalHead(timeoutMs:)``. While any wait,
    /// the poll does not hold its read open: a 25 s long poll per head read
    /// made every step of an invite-link answer take about 25 s.
    private var headWaiters = 0
    /// The long poll out now, cut short when a caller starts waiting.
    private var pollFetchTask: Task<FetchResult, Error>?
    /// Set when ``pollFetchTask`` was cancelled for a waiter, so the poll
    /// reads again at once instead of backing off.
    private var pollKicked = false

    public init() {
        // ADR-0081 — a DeviceGroup commit of this device landed through a
        // handle of its own: reload the long-lived handle from the snapshot
        // the outbox made current. Posted on the main actor (the outbox is
        // main-actor), delivered synchronously.
        changedHereObserver = NotificationCenter.default.addObserver(
            forName: .lakkoDeviceGroupChangedHere, object: nil, queue: nil
        ) { [weak self] note in
            guard let account = note.userInfo?["accountIdB64"] as? String else { return }
            MainActor.assumeIsolated {
                self?.reloadFromDisk(accountIdB64: account)
            }
        }
        restoreObserver = NotificationCenter.default.addObserver(
            forName: .lakkoSyncedStateRestored, object: nil, queue: .main
        ) { [weak self] note in
            guard let account = note.userInfo?["accountIdB64"] as? String,
                  let bytes = note.userInfo?["bytes"] as? Data else { return }
            MainActor.assumeIsolated {
                self?.foldRestored(bytes: bytes, accountIdB64: account)
            }
        }
    }

    // MARK: - Lifecycle

    /// Bootstrap the cross-device sync runtime for the freshly-
    /// authenticated session. Idempotent for the same account.
    /// Called from `AppRoot` whenever `identitySecrets`
    /// transitions to non-nil.
    public func bootstrap(secrets: IdentitySecrets) {
        let accountIdB64 = base64Encode(secrets.accountId)
        if activeAccountIdB64 == accountIdB64 { return }
        if activeAccountIdB64 != nil {
            cancelPollAndDropHandles()
        }
        deviceRemoved = false
        activeAccountIdB64 = accountIdB64
        activeAccountId = secrets.accountId
        let seed: Data
        do {
            seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(
                accountIdB64: accountIdB64
            )
        } catch {
            NSLog("DeviceGroupSync bootstrap: seed failure %@", error.localizedDescription)
            activeAccountIdB64 = nil
            return
        }
        let ensured: DeviceGroupLifecycle.Ensured
        do {
            ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
                accountIdBytes: secrets.accountId,
                accountIdB64: accountIdB64,
                deviceIdentitySeed: seed
            )
        } catch is DeviceGroupLifecycle.NotJoined {
            // ADR-0081 decision 4 — no usable DeviceGroup state: found only
            // when the directory lists no other own device, rejoin otherwise.
            startFoundOrRejoin(
                accountIdBytes: secrets.accountId, accountIdB64: accountIdB64, deviceSeed: seed)
            return
        } catch {
            NSLog("DeviceGroupSync bootstrap: ensureDeviceGroup %@", error.localizedDescription)
            activeAccountIdB64 = nil
            return
        }
        adopt(ensured, accountIdB64: accountIdB64)
    }

    /// Take `ensured` as this session's DeviceGroup handle and start
    /// following it: the removed check, the synced state, the roster, the
    /// poll loop.
    private func adopt(_ ensured: DeviceGroupLifecycle.Ensured, accountIdB64: String) {
        identity = ensured.identity
        group = ensured.group
        handleGeneration = ensured.generation
        loadAdmissions(accountIdB64: accountIdB64)
        // Removed while the app was closed: the persisted snapshot already
        // carries the commit that removed this device. Nothing is polled,
        // folded or persisted for it; the app root shows the removed screen.
        if noteDeviceGroupActivity(RemovedDevice.readActive { try ensured.group.isActive() }) {
            return
        }

        // Seed the fold from this device's own disk BEFORE the poll starts.
        // The journal cannot refill it: the poll runs from cursor 0, but the
        // MLS snapshot was restored above, so every row it re-fetches is one
        // this state has already consumed. A backup restore merged its state
        // into the same store before this ran (``SyncedStateStore``).
        syncedState = SyncedStateStore.load(accountIdB64: accountIdB64)
        pending = PendingSyncTracker(accountIdB64: accountIdB64)
        if let maxHlc = try? syncedState.maxHlc() { Hlc.observe(maxHlc) }
        refreshDerived()
        ready = true
        onSyncedStateReady?()
        // UX 17.5 — opening Settings > Devices acknowledges the pending
        // devices for the account through this repository.
        NewDeviceNoticeCenter.shared.setAcknowledger(accountIdB64: accountIdB64) { [weak self] keys in
            self?.acknowledgeDevices(keys)
        }
        // UX 17.5 — the roster as restored: the first reading on this device
        // seeds the acknowledged set; a later one may raise the notice for a
        // device added while the app was closed and already applied.
        reportRoster(group: ensured.group)
        DeviceRosterChanges.note(accountIdB64: accountIdB64)
        // ADR-0080 — this account's device cards, sent through this
        // repository's DeviceGroup `ServiceEnvelope`.
        DeviceCardCenter.shared.bind(accountIdB64: accountIdB64) { [weak self] envelope in
            await self?.sendServiceEnvelope(envelope)
        }
        // A touch before the account bound was not noted: an account bound
        // while the app is in front counts as the person's action once.
        DeviceCardCenter.shared.noteForeground()

        let groupId: Data
        do {
            groupId = try ensured.group.groupId()
        } catch {
            NSLog("DeviceGroupSync bootstrap: groupId %@", error.localizedDescription)
            return
        }
        pollTask = Task { [weak self] in
            // ADR-0053 point 2 — claim-on-open before the first read (also the
            // claim of a DeviceGroup this bootstrap just founded).
            if let group = self?.group, let identity = self?.identity {
                await DeviceGroupDsAccess.claim(
                    group: group, identity: identity, accountIdB64: accountIdB64, ds: DsClient())
            }
            await self?.runPollLoop(
                accountIdB64: accountIdB64,
                groupId: groupId
            )
        }
    }

    /// Tear down the runtime — cancel poll loop, drop FFI
    /// handles, clear every reactive map.
    public func clear() {
        // The persisted state stays for the next sign-in of the same account
        // (2026-09-30): it is filed per account (``SyncedStateStore``, the
        // pending flag of ``PendingSyncTracker``), so no other account can
        // read it, and deleting it here lost the account's folders, flags and
        // drafts on every logout, since the restored DeviceGroup snapshot has
        // already consumed the journal rows a re-login would re-poll. A
        // debounced write still in flight is flushed rather than dropped.
        // ``AccountLocalDataWipe`` is what deletes them.
        if let closing = activeAccountIdB64, persistTask != nil, !deviceRemoved {
            SyncedStateStore.save(syncedState, accountIdB64: closing)
        }
        cancelPollAndDropHandles()
        NewDeviceNoticeCenter.shared.bind(accountIdB64: nil)
        DeviceCardCenter.shared.unbind()
        pendingServiceEnvelopes = [:]
        activeAccountIdB64 = nil
        activeAccountId = nil
        ready = false
        deviceRemoved = false
        otherDeviceSignedIn = nil
        syncedState = SyncedState()
        pending = nil
        refreshDerived()
    }

    /// Publish the muted set into the App Group container
    /// (``ConversationMuteMirror``), so the three call paths that cannot reach
    /// this repository still know not to ring.
    ///
    /// UNLIKE the snapshot write below, this is NOT debounced. The debounce
    /// buys an AES seal and a file replace; a `UserDefaults` write costs
    /// neither, and half a second is long enough for the call a person just
    /// muted to ring anyway. Whole-set rather than per-row, because an unmute
    /// on a sibling device has to REMOVE a row.
    private func mirrorMutedConversations() {
        // Per account: a `nil` account (the sign-out teardown empties the
        // flags after unbinding) writes nothing, so logout keeps the rows.
        ConversationMuteMirror.replaceAll(
            accountIdB64: activeAccountIdB64,
            mutedGroupIdsHex: Set(flags.filter { $0.value.muted }.keys))
    }

    /// Write the fold to disk, shortly.
    ///
    /// Debounced because a draft moves on a keystroke and each write is an
    /// AES seal plus a file replace.
    private func scheduleSyncStatePersist() {
        persistTask?.cancel()
        if deviceRemoved { return }
        persistTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 500_000_000)
            guard !Task.isCancelled else { return }
            self?.persistSyncState()
        }
    }

    /// Write the fold to disk. Best-effort: the in-memory state stays good
    /// and the next change rewrites the envelope.
    private func persistSyncState() {
        guard !deviceRemoved, let accountIdB64 = activeAccountIdB64 else { return }
        SyncedStateStore.save(syncedState, accountIdB64: accountIdB64)
    }

    /// The fold changed: rebuild the published projections and schedule the
    /// write.
    private func stateChanged() {
        refreshDerived()
        scheduleSyncStatePersist()
    }

    /// Rebuild every published projection from ``syncedState``. Assignments
    /// that would not change a value are skipped, so an unrelated fold does
    /// not re-render every screen that observes one map.
    private func refreshDerived() {
        let s = syncedState
        QuickReactionSetting.shared.adopt((try? s.quickReaction()).flatMap { $0 })
        var d: [String: DraftState] = [:]
        for r in (try? s.allDrafts()) ?? [] {
            d[groupIdHex(r.groupId)] = DraftState(
                text: r.text, replyToId: r.replyToId, updatedAtHlc: r.hlc)
        }
        var f: [String: FlagsState] = [:]
        for r in (try? s.allFlags()) ?? [] {
            f[groupIdHex(r.groupId)] = FlagsState(
                pinned: r.pinned, muted: r.muted, archived: r.archived,
                unreadOverride: r.unreadOverride, updatedAtHlc: r.hlc)
        }
        var c: [String: ReadCursorState] = [:]
        for r in (try? s.allReadCursors()) ?? [] {
            c[groupIdHex(r.groupId)] = ReadCursorState(
                upToMessageId: r.upToMessageId, readAtMs: r.readAtMs, updatedAtHlc: r.hlc)
        }
        let o = (try? s.conversationOrder()).flatMap { $0 }.map {
            OrderState(groupIds: $0.groupIds, updatedAtHlc: $0.hlc)
        }
        let folderList = (try? s.folders()) ?? []
        let lineageList = ((try? s.allLineage()) ?? []).map {
            LineageEntry(
                successorHex: groupIdHex($0.successorGroupId),
                rootHex: groupIdHex($0.rootConversationId),
                hlc: $0.hlc)
        }
        if d != drafts { drafts = d }
        if f != flags { flags = f }
        if c != readCursors { readCursors = c }
        if o != order { order = o }
        if folderList != folders { folders = folderList }
        let savedList = SavedMessages.ordered((try? s.saved()) ?? [])
        if savedList != saved { saved = savedList }
        if lineageList != lineage { lineage = lineageList }
        let ends = (try? s.endedConversations()) ?? []
        if ends != endedConversations { endedConversations = ends }
        let address = (try? s.recoveryEmail()).flatMap { $0 }
        if address != recoveryEmail { recoveryEmail = address }
        let name = (try? s.ownUsername()).flatMap { $0 }
        if name != ownUsername { ownUsername = name }
        let links = (try? s.allLinkRecords()) ?? []
        if links != linkRecords { linkRecords = links }
        let isPending = pending?.isPending ?? false
        if pendingSync != isPending { pendingSync = isPending }
        // UX 17.5 / 17.6 — the synced acknowledged set ends the new-device
        // warning on this device for every device in it.
        if let account = activeAccountIdB64 {
            NewDeviceNoticeCenter.shared.setAcknowledged(
                accountIdB64: account, keys: (try? s.acknowledgedDevices()) ?? [])
        }
    }

    /// Fold a restored backup state in, when it is this account's.
    private func foldRestored(bytes: Data, accountIdB64: String) {
        guard accountIdB64 == activeAccountIdB64, !deviceRemoved,
              let incoming = try? SyncedState.fromBytes(bytes: bytes) else { return }
        if SyncedStateMerge.merge(incoming, into: syncedState) {
            if let maxHlc = try? syncedState.maxHlc() { Hlc.observe(maxHlc) }
            stateChanged()
        }
    }

    private func cancelPollAndDropHandles() {
        binding.advance()
        pollTask?.cancel()
        pollTask = nil
        resendTask?.cancel()
        resendTask = nil
        persistTask?.cancel()
        persistTask = nil
        rejoinTask?.cancel()
        rejoinTask = nil
        identity = nil
        group = nil
        handleGeneration = nil
        admissions = nil
        lastKnockCheckAtMs = 0
        knockCursor = 0
    }

    // MARK: - Removed device (docs/ux/second-device.md section 3.1)

    /// Act on a read of `DeviceGroup.isActive()` for the held handle. An
    /// explicit `false` means another device removed this one: stop the poll
    /// loop for good this session and every task that would send or persist,
    /// and publish ``deviceRemoved``. The handles stay held until ``clear()``
    /// drops them on sign-out. `true` and a failed read (`nil`) change
    /// nothing. Returns whether the device is now removed.
    @discardableResult
    func noteDeviceGroupActivity(_ active: Bool?) -> Bool {
        guard RemovedDevice.pollStepAfterBatch(active) == .stopRemoved else { return deviceRemoved }
        if !deviceRemoved {
            NSLog("DeviceGroupSync: this device was removed from the account; sync stopped")
        }
        deviceRemoved = true
        pollTask?.cancel()
        pollTask = nil
        resendTask?.cancel()
        resendTask = nil
        persistTask?.cancel()
        persistTask = nil
        rejoinTask?.cancel()
        rejoinTask = nil
        return true
    }

    /// Whether a poll loop is running for this session.
    var isPolling: Bool { pollTask != nil }

    /// Test seam: stand in for the poll loop ``bootstrap(secrets:)`` starts,
    /// so a test can observe that a removal stops it.
    func adoptPollTaskForTesting(_ task: Task<Void, Never>) {
        pollTask = task
    }

    // MARK: - Lookup helpers

    public func draft(for groupId: Data) -> DraftState? {
        drafts[groupIdHex(groupId)]
    }

    public func flags(for groupId: Data) -> FlagsState? {
        flags[groupIdHex(groupId)]
    }

    public func readCursor(for groupId: Data) -> ReadCursorState? {
        readCursors[groupIdHex(groupId)]
    }

    // MARK: - Outbound send paths

    /// Compose + publish a `Draft` payload. Empty `text` clears
    /// the receivers' draft for the same group.
    public func setDraft(
        groupId: Data,
        text: String,
        replyToId: Data = Data()
    ) async throws {
        try await write(.draft(groupId: groupId, text: text, replyToId: replyToId))
    }

    /// Compose + publish a `Flags` payload. Whole-snapshot
    /// last-write-wins.
    public func setFlags(
        groupId: Data,
        pinned: Bool,
        muted: Bool,
        archived: Bool,
        unreadOverride: Bool
    ) async throws {
        try await write(.flags(
            groupId: groupId,
            pinned: pinned,
            muted: muted,
            archived: archived,
            unreadOverride: unreadOverride))
        markBackupDirty()
    }

    /// Compose + publish a `ReadCursor` payload.
    public func setReadCursor(
        groupId: Data,
        upToMessageId: Data,
        readAtMs: UInt64
    ) async throws {
        try await write(.readCursor(
            groupId: groupId, upToMessageId: upToMessageId, readAtMs: readAtMs))
    }

    /// Compose + publish an `Order` payload.
    public func setOrder(groupIds: [Data]) async throws {
        try await write(.order(groupIds: groupIds))
    }

    /// ADR-0058 — publish a folder edit: only the registers it changed, each
    /// already stamped by the caller (``mintHlc()``). Folded here first, so
    /// the edit shows at once whether or not the publish goes through.
    public func applyFolderChanges(_ changes: FolderChangeSet) async throws {
        if ChatFolders.isEmpty(changes) { return }
        try await write(.folderChanges(changes: changes))
        markBackupDirty()
    }

    /// ADR-0065 section 4 — publish saves and removals of saved messages, in
    /// change sets of at most ``savedMaxPerChangeSet()`` (one journal row).
    public func applySaved(_ registers: [SavedMessageRegister]) async throws {
        let size = max(1, Int(savedMaxPerChangeSet()))
        var start = 0
        while start < registers.count {
            let chunk = Array(registers[start..<min(registers.count, start + size)])
            try await applyFolderChanges(FolderChangeSet(
                metas: [], members: [], pins: [], removed: [], order: nil, saved: chunk))
            start += size
        }
    }

    // MARK: - ADR-0087 section 9: the end of a membership

    /// Step 2. A `nil` compose means there is nothing to tell the other
    /// devices (past the client limit, or the register already says so), and
    /// that never stops the wipe. A publish that failed is the state's pending
    /// re-send, as for every other write. A device removed from the account
    /// has nobody to tell.
    public func publishConversationEnded(
        _ conversation: FolderConversationRef, epoch: UInt64
    ) async -> Bool {
        if deviceRemoved { return true }
        guard ready else { return false }
        do {
            guard let register = try syncedState.composeConversationEnded(
                hlc: Hlc.mint(), conversation: conversation, epoch: epoch) else { return true }
            try await writeConversationEnds([register])
            return true
        } catch {
            NSLog("conversation end not written: %@", error.localizedDescription)
            return false
        }
    }

    /// Rule 3 — a join clears an end this state holds; a first join writes
    /// nothing. A device that could not write this now answers the register
    /// when it next reads it (rule 4).
    public func publishConversationJoined(
        _ conversation: FolderConversationRef, epoch: UInt64
    ) async {
        guard ready, !deviceRemoved else { return }
        do {
            guard let register = try syncedState.composeConversationJoined(
                hlc: Hlc.mint(), conversation: conversation, epoch: epoch) else { return }
            try await writeConversationEnds([register])
        } catch {
            NSLog("conversation join not written: %@", error.localizedDescription)
        }
    }

    private func writeConversationEnds(_ registers: [ConversationEndRegister]) async throws {
        try await applyFolderChanges(FolderChangeSet(
            metas: [], members: [], pins: [], removed: [], order: nil,
            saved: [], conversationEnds: registers))
    }

    /// Clear what the synced state holds of `conversations`, through each
    /// register's own tombstone, and only where something is held: a device
    /// that hears the end after the device that wiped first writes nothing.
    /// Profile rows are not tied to a group and stay.
    public func clearConversation(_ conversations: [FolderConversationRef]) async -> Bool {
        if deviceRemoved { return true }
        guard ready else { return false }
        do {
            for conversation in conversations {
                let id = conversation.id
                let key = groupIdHex(id)
                if let draft = drafts[key], !draft.text.isEmpty {
                    try await setDraft(groupId: id, text: "")
                }
                if flags[key].map({ $0.pinned || $0.muted || $0.archived || $0.unreadOverride }) == true {
                    try await setFlags(
                        groupId: id, pinned: false, muted: false, archived: false, unreadOverride: false)
                }
                if let cursor = readCursors[key], !cursor.upToMessageId.isEmpty {
                    try await setReadCursor(groupId: id, upToMessageId: Data(), readAtMs: 0)
                }
            }
            let ids = Set(conversations.map(\.id))
            if let order, order.groupIds.contains(where: { ids.contains($0) }) {
                try await setOrder(groupIds: order.groupIds.filter { !ids.contains($0) })
            }
            let changes = Self.conversationClearChanges(
                folders: folders, conversations: ids, hlc: Hlc.mint())
            try await applyFolderChanges(changes)
            try await applySaved(try saved
                .filter { ids.contains($0.conversation.id) }
                .map { try composeSavedRemoval(
                    hlc: Hlc.mint(), conversation: $0.conversation, messageId: $0.messageId) })
            return true
        } catch {
            NSLog("conversation registers not cleared: %@", error.localizedDescription)
            return false
        }
    }

    /// The folder edit that takes `conversations` out of every folder: the
    /// hand-added and hand-excluded registers back to `none`, and the pins.
    /// A folder that matches by kind stops showing it with the row itself.
    nonisolated static func conversationClearChanges(
        folders: [FolderView], conversations ids: Set<Data>, hlc: UInt64
    ) -> FolderChangeSet {
        var members: [FolderMemberRegister] = []
        var pins: [FolderPinsRegister] = []
        for folder in folders {
            for ref in (folder.included + folder.excluded) where ids.contains(ref.id) {
                members.append(FolderMemberRegister(
                    hlc: hlc, folderId: folder.folderId, conversation: ref, state: .none))
            }
            if folder.pinned.contains(where: { ids.contains($0.id) }) {
                pins.append(FolderPinsRegister(
                    hlc: hlc, folderId: folder.folderId,
                    pinned: folder.pinned.filter { !ids.contains($0.id) }))
            }
        }
        return FolderChangeSet(metas: [], members: members, pins: pins, removed: [], order: nil)
    }

    /// Blob ids of every saved message's files (for the keep tick).
    public func savedBlobIds() -> [Data] { (try? syncedState.savedBlobIds()) ?? [] }

    /// ADR-0058 section 5.2 — the backup carries this state, so a person's
    /// own folder or flag edit is a change worth backing up. Read cursors and
    /// drafts move too often to count on their own; they ride along with the
    /// next upload.
    private func markBackupDirty() {
        guard let account = activeAccountIdB64 else { return }
        AutoBackupStore.markDirty(
            accountIdB64: account, nowMs: Int64(Date().timeIntervalSince1970 * 1000))
    }

    /// A fresh stamp for a register, from the same clock every write uses.
    public func mintHlc() -> UInt64 { Hlc.mint() }

    /// One local write: fold it into this device's state (a device never
    /// receives its own DeviceGroup messages back), then publish it. A
    /// publish that fails marks the state pending instead of failing the
    /// write: it has already happened here, and it travels with the re-send
    /// after the next poll fetch that succeeds.
    private func write(_ body: DeviceGroupBody) async throws {
        guard ready, !deviceRemoved, activeAccountIdB64 != nil else {
            throw NSError(
                domain: "DeviceGroupSync",
                code: 1,
                userInfo: [NSLocalizedDescriptionKey: "Sync not ready"]
            )
        }
        let hlc = Hlc.mint()
        let own = (try? group?.ownLeafIndex()) ?? 0
        let local = ParsedDeviceGroupPayload(hlc: hlc, senderLeaf: own, body: body)
        if try syncedState.applyPayload(payload: local) {
            stateChanged()
        }
        do {
            try await sendBody(body, hlc: hlc)
        } catch {
            NSLog("DeviceGroupSync publish failed, pending: %@", error.localizedDescription)
            pending?.markFailed()
            refreshDerived()
        }
    }

    /// After a poll fetch succeeded: if a publish failed earlier (in this
    /// process or before a relaunch), re-send the whole state. The flag
    /// clears only when no publish failed while the re-send ran.
    private func flushPending() async {
        guard let tracker = pending, tracker.isPending else { return }
        let token = tracker.beginFlush()
        await sendSnapshot()
        tracker.finishFlush(token)
        refreshDerived()
    }

    /// ADR-0060 section 7 — send a `lakko-contacts` envelope (the Visibility
    /// setting or the ReachState counters) to the account's other devices as a
    /// DeviceGroup `ServiceEnvelope`. The envelope bytes go verbatim, so the
    /// receiver orders it by the stamp it was composed with; the HLC minted
    /// here only orders the DeviceGroup row. Best effort: when the handle is
    /// not ready or the publish fails, the envelope is kept (one per kind) and
    /// retried by the poll loop.
    public func sendServiceEnvelope(_ envelope: Data) async {
        // An envelope that arrives while no account is bound was composed
        // for the account that just left (its sender hops through a `Task`).
        // Kept, it would be flushed into whichever account's DeviceGroup
        // bootstraps next.
        guard activeAccountIdB64 != nil else { return }
        let slot = Self.serviceEnvelopeSlot(envelope)
        pendingServiceEnvelopes[slot] = envelope
        await flushPendingServiceEnvelopes()
    }

    /// Every service envelope a device newly admitted to the DeviceGroup is
    /// handed: the Visibility setting and the ReachState (ADR-0060 section 7)
    /// and the own profile (ADR-0048 section 4a), each verbatim with its
    /// stored stamp, and each only when it was ever set.
    static func handOverEnvelopes(
        accountIdB64: String, reach: ReachCenter, profiles: ProfileDirectory
    ) -> [Data] {
        reach.deviceEnvelopes(accountIdB64: accountIdB64)
            + profiles.ownDeviceEnvelopes(accountIdB64: accountIdB64)
    }

    /// The pending-send slot of `envelope`: its dispatched kind, so a newer
    /// envelope of the same kind replaces an older unsent one.
    static func serviceEnvelopeSlot(_ envelope: Data) -> String {
        // ADR-0073 R.5 — every lineage register is its own slot: one never
        // replaces another.
        if isLineageEnvelope(bytes: envelope) { return "lineage:" + envelope.base64EncodedString() }
        // ADR-0075 — one register: a newer unsent one replaces an older.
        if isRecoveryEmailEnvelope(bytes: envelope) { return "recovery-email" }
        // ADR-0076 — likewise the own-username register.
        if isOwnUsernameEnvelope(bytes: envelope) { return "own-username" }
        // Reactions-and-emoji spec section 6 (D3) — the quick-reaction register.
        if isQuickReactionEnvelope(bytes: envelope) { return "quick-reaction" }
        // UX 17.6 — every device acknowledgement is its own slot.
        if isDeviceAckEnvelope(bytes: envelope) { return "device-ack:" + envelope.base64EncodedString() }
        // ADR-0079 — every link record is its own slot.
        if isLinkRecordEnvelope(bytes: envelope) { return "link-record:" + envelope.base64EncodedString() }
        guard let kind = try? dispatchIncomingEnvelope(plaintext: envelope).kind else {
            return envelope.base64EncodedString()
        }
        return String(describing: kind)
    }

    private func flushPendingServiceEnvelopes() async {
        guard ready, group != nil, !deviceRemoved else { return }
        for (slot, envelope) in pendingServiceEnvelopes {
            do {
                try await publish { group, identity in
                    try group.createServiceEnvelopeMessage(
                        identity: identity, envelope: envelope, hlc: Hlc.mint())
                }
                // A newer envelope may have taken the slot during the await.
                if pendingServiceEnvelopes[slot] == envelope {
                    pendingServiceEnvelopes[slot] = nil
                }
            } catch {
                NSLog("ADR-0060: DeviceGroup service envelope not sent, retried: %@",
                      error.localizedDescription)
                return
            }
        }
    }

    /// Whether `envelope` dispatches as a `key-rotation` envelope.
    static func isKeyRotation(_ envelope: Data) -> Bool {
        (try? dispatchIncomingEnvelope(plaintext: envelope).kind) == .keyRotation
    }

    // MARK: - ADR-0074 section 3.2: the rotating device

    /// The epoch of the live DeviceGroup handle (the one every message of
    /// this repository is sealed at).
    public func currentEpoch() throws -> UInt64 {
        guard let group, let identity, !deviceRemoved else {
            throw NSError(
                domain: "DeviceGroupSync", code: 1,
                userInfo: [NSLocalizedDescriptionKey: "Sync not ready"])
        }
        return try group.dsAccessSigner(identity: identity).epoch()
    }

    /// Seal `envelope` (a `key-rotation` envelope) as ONE application message
    /// of the account's DeviceGroup, at the live handle's current epoch, and
    /// publish it on the DeviceGroup journal: the only destination it ever
    /// has. Returns the epoch the emitted MLS message is framed at, read off
    /// the message itself, which is what ``KeyRotationAnnounce`` checks
    /// against the removal's floor.
    ///
    /// Unlike ``sendServiceEnvelope(_:)`` this is not best effort and keeps
    /// nothing for a retry: the rotation stops when it throws.
    public func sendKeyRotationEnvelope(_ envelope: Data) async throws -> UInt64 {
        guard Self.isKeyRotation(envelope) else {
            throw NSError(
                domain: "DeviceGroupSync", code: 3,
                userInfo: [NSLocalizedDescriptionKey: "Not a key-rotation envelope"])
        }
        var sealedEpoch: UInt64?
        try await publish { group, identity in
            let wire = try group.createServiceEnvelopeMessage(
                identity: identity, envelope: envelope, hlc: Hlc.mint())
            sealedEpoch = epochOfMessage(bytes: wire)
            return wire
        }
        guard let sealedEpoch else {
            throw NSError(
                domain: "DeviceGroupSync", code: 4,
                userInfo: [NSLocalizedDescriptionKey: "The sealed message carries no epoch"])
        }
        return sealedEpoch
    }

    /// Wait for the DeviceGroup handle of the session that just landed: a
    /// device with no DeviceGroup yet reads the directory first. Returns
    /// whether the handle is there. Chosen, not measured: ten seconds.
    @discardableResult
    public func waitForHandle(timeoutMs: Int64 = 10_000) async -> Bool {
        let start = Int64(Date().timeIntervalSince1970 * 1000)
        while group == nil, !deviceRemoved, activeAccountIdB64 != nil,
              Int64(Date().timeIntervalSince1970 * 1000) - start < timeoutMs {
            try? await Task.sleep(nanoseconds: 100_000_000)
            if Task.isCancelled { break }
        }
        return group != nil
    }

    /// How long ``waitForJournalHead(timeoutMs:)`` waits by default: one long
    /// poll (25 s) and a margin. Chosen, not measured.
    public static let journalHeadTimeoutMs: Int64 = 30_000

    /// ADR-0074 section 3.3, "order on a superseded device" — wait until the
    /// poll loop has read the DeviceGroup journal to its head once more (a
    /// fetch that completed after this call and found nothing further). The
    /// DS does not use session tokens, so a device whose session is
    /// superseded can still do this. `false` when the journal could not be
    /// read to its head in time (offline, a journal that answers 401).
    public func waitForJournalHead(timeoutMs: Int64 = DeviceGroupSyncRepository.journalHeadTimeoutMs) async -> Bool {
        guard ready, group != nil, !deviceRemoved else { return false }
        let start = Int64(Date().timeIntervalSince1970 * 1000)
        headWaiters += 1
        defer { headWaiters -= 1 }
        // A long poll already out started before this call: cut it short so
        // the next read starts now and does not wait.
        if let out = pollFetchTask {
            pollKicked = true
            out.cancel()
        }
        // A read that STARTED after this call, so it carries every row
        // published before it.
        while Int64(Date().timeIntervalSince1970 * 1000) - start < timeoutMs {
            if lastDrainedFetchStartedAtMs > start { return true }
            try? await Task.sleep(nanoseconds: 100_000_000)
            if Task.isCancelled { return false }
        }
        return lastDrainedFetchStartedAtMs > start
    }

    private func sendBody(_ body: DeviceGroupBody, hlc: UInt64) async throws {
        try await publish { group, identity in
            try group.createDevicegroupMessage(identity: identity, body: body, hlc: hlc)
        }
    }

    /// Encrypt with `compose` under the DeviceGroup's current epoch and
    /// publish it on the DeviceGroup journal. Returns the DS message id.
    ///
    /// A handle that fell behind the disk (another handle of this device
    /// committed) stops ``publishOnce(_:)`` before anything is sent. It is
    /// reloaded there, and the message is sealed once more on the reloaded
    /// handle, so a publish is not lost to that race.
    @discardableResult
    private func publish(
        _ compose: (DeviceGroup, Identity) throws -> Data
    ) async throws -> Int64 {
        try await Self.retryingOnceOnStaleHandle { try await publishOnce(compose) }
    }

    /// `attempt`, and one more time when it stopped on a handle that fell
    /// behind the disk. Any other error, and a second stale handle, is the
    /// caller's.
    static func retryingOnceOnStaleHandle<T>(_ attempt: () async throws -> T) async throws -> T {
        do {
            return try await attempt()
        } catch is DeviceGroupSnapshotStore.SnapshotChanged {
            NSLog("DeviceGroupSync: the snapshot moved under this handle; sealing again on the reloaded one")
            return try await attempt()
        }
    }

    private func publishOnce(
        _ compose: (DeviceGroup, Identity) throws -> Data
    ) async throws -> Int64 {
        if deviceRemoved {
            throw NSError(
                domain: "DeviceGroupSync",
                code: 2,
                userInfo: [NSLocalizedDescriptionKey: "This device was removed from the account"]
            )
        }
        guard let group = group, let identity = identity,
              let accountIdB64 = activeAccountIdB64 else {
            throw NSError(
                domain: "DeviceGroupSync",
                code: 1,
                userInfo: [NSLocalizedDescriptionKey: "Sync not ready"]
            )
        }
        let wireBytes = try compose(group, identity)
        // The message took a step of this device's sender ratchet. That
        // state is on disk before the ciphertext is published, or nothing is
        // published: a failed write, a session that ended, a handle of an
        // earlier session, or (ADR-0081 decision 3) a handle another one
        // wrote past all stop here.
        do {
            handleGeneration = try DeviceGroupLifecycle.persistSnapshotOrThrow(
                identity: identity, accountIdB64: accountIdB64, expectedGeneration: handleGeneration)
        } catch let changed as DeviceGroupSnapshotStore.SnapshotChanged {
            reloadFromDisk(accountIdB64: accountIdB64)
            throw changed
        }
        let groupId = try group.groupId()
        let ds = DsClient()
        // G-13 (ADR-0015): wrap in the journal envelope (application messages
        // carry no attestation).
        let envelope = DeviceGroupJournalEnvelope.encode(
            message: wireBytes,
            attestation: nil
        )
        let published = try await ds.publishGroup(
            groupId: groupId,
            payload: envelope,
            auth: DeviceGroupDsAccess.auth(
                group: group, identity: identity, accountIdB64: accountIdB64, ds: ds)
        )
        // Already on disk before the publish; nothing to write again.
        return published.id
    }

    /// ADR-0079 (reworked 2026-10-17) — seal and post this device's claim of
    /// a link request into the account's DeviceGroup, on its own
    /// `ServiceEnvelope` carrier. `beforePublish` gets the sealed bytes (hex)
    /// and must record them durably (`false` aborts the publish). The
    /// publish answer files nothing: the receive loop judges the row at its
    /// position (``judgeOwnLinkRow``; it counts only when sealed at the
    /// journal's epoch there). Returns the DS message id.
    @discardableResult
    public func postLinkClaim(
        linkId: Data,
        requesterKey: Data,
        beforePublish: (String) -> Bool
    ) async throws -> Int64 {
        let envelope = try composeLinkClaimEnvelope(linkId: linkId, requesterAccountKey: requesterKey)
        return try await publishRecorded(envelope: envelope, beforePublish: beforePublish)
    }

    /// ADR-0079 (order of 2026-10-17) — publish the link-founded note (the
    /// request, the one-to-one founded for it, its Welcome) into the
    /// account's DeviceGroup, BEFORE any Welcome is written. `beforePublish`
    /// records the sealed bytes first, so the receive loop judges the row at
    /// its position. Returns the DS message id; throws when the publish
    /// failed.
    @discardableResult
    public func postLinkFounded(
        linkId: Data, requesterKey: Data, groupId: Data, welcome: Data,
        beforePublish: (String) -> Bool
    ) async throws -> Int64 {
        let envelope = try composeLinkFoundedEnvelope(
            linkId: linkId, requesterAccountKey: requesterKey, groupId: groupId, welcome: welcome)
        return try await publishRecorded(envelope: envelope, beforePublish: beforePublish)
    }

    private func publishRecorded(envelope: Data, beforePublish: (String) -> Bool) async throws -> Int64 {
        try await publish { group, identity in
            let wire = try group.createServiceEnvelopeMessage(identity: identity, envelope: envelope, hlc: Hlc.mint())
            guard beforePublish(LinkClaims.hex(wire)) else {
                throw NSError(
                    domain: "DeviceGroupSync",
                    code: 3,
                    userInfo: [NSLocalizedDescriptionKey: "Link row not recorded"]
                )
            }
            return wire
        }
    }

    /// ADR-0079 — another device's link-founded note, taken in. Kept (the
    /// earliest per request) only from a sender leaf the roster this row was
    /// sealed under resolves (C-68). `true` when the envelope was a note
    /// (kept or dropped).
    func receiveLinkFounded(_ envelope: Data, senderLeaf: UInt32?, group g: DeviceGroup, messageId: Int64) -> Bool {
        guard isLinkFoundedEnvelope(bytes: envelope) else { return false }
        guard let note = try? decodeLinkFoundedEnvelope(plaintext: envelope) else { return true }
        guard let senderLeaf, let roster = try? g.devices(),
              roster.contains(where: { $0.leafIndex == senderLeaf }),
              let account = activeAccountIdB64 else { return true }
        let changed = LinkClaims.Record(accountIdB64: account).recordNote(
            LinkClaims.requestKeyHex(linkId: note.linkId, requesterKey: note.requesterAccountKey),
            note: LinkClaims.HeldNote(
                id: messageId, groupIdB64: base64Encode(note.groupId), welcomeB64: base64Encode(note.welcome)),
            nowMs: Int64(Date().timeIntervalSince1970 * 1000))
        if changed { linkClaimsVersion += 1 }
        return true
    }

    /// ADR-0079 — this device's DeviceGroup leaf key and the current
    /// roster's leaf keys, the inputs of the claim decision. `nil` while no
    /// handle is held or this device was removed.
    public func linkClaimRoster() -> (ownKey: Data, roster: [Data])? {
        guard !deviceRemoved, let g = group, let devices = try? g.devices(), !devices.isEmpty,
              let ownLeaf = try? g.ownLeafIndex(),
              let ownKey = devices.first(where: { $0.leafIndex == ownLeaf })?.signaturePublicKey
        else { return nil }
        return (ownKey, devices.map(\.signaturePublicKey))
    }

    /// ADR-0079 (order of 2026-10-17) — the journal's epochs for one batch:
    /// the record kept next to the claim record, or one starting at the
    /// group's epoch when none is held for this DeviceGroup.
    func loadJournalEpochs(_ g: DeviceGroup) -> (groupIdHex: String, epochs: FfiJournalEpochs)? {
        guard let account = activeAccountIdB64, let groupId = try? g.groupId() else { return nil }
        let groupIdHex = LinkClaims.hex(groupId)
        if let stored = LinkClaims.Record(accountIdB64: account).journal(groupIdHex: groupIdHex) {
            return (groupIdHex, FfiJournalEpochs(
                base: stored.base, commitRowIds: stored.commitRowIds, commitEpochsAfter: stored.commitEpochsAfter))
        }
        guard let epoch = try? g.epoch() else { return nil }
        let fresh = journalEpochsStartingAt(base: epoch)
        // Kept at once: the start must not move with the group's epoch on
        // later batches (an own commit merged early would then make an own
        // claim sealed before it look straddled).
        LinkClaims.Record(accountIdB64: account).setJournal(LinkClaims.Record.StoredJournal(
            groupIdHex: groupIdHex, base: fresh.base,
            commitRowIds: fresh.commitRowIds, commitEpochsAfter: fresh.commitEpochsAfter))
        return (groupIdHex, fresh)
    }

    /// ADR-0079 — a journal row carrying one of this device's recorded sealed
    /// claims or notes (its own application message cannot be decrypted
    /// here) is judged at its position: it counts only when sealed at the
    /// journal's epoch there. `true` when it was one.
    func judgeOwnLinkRow(_ mlsMessage: Data, messageId: Int64, journal: FfiJournalEpochs) -> Bool {
        guard let account = activeAccountIdB64 else { return false }
        let record = LinkClaims.Record(accountIdB64: account)
        guard record.hasUnmatchedWires() else { return false }
        let counts = epochOfMessage(bytes: mlsMessage).map {
            journalRowCounts(epochs: journal, rowId: UInt64(max(0, messageId)), sealedEpoch: $0)
        } ?? false
        guard record.judgeOwnRow(
            wireHex: LinkClaims.hex(mlsMessage), messageId: messageId, counts: counts) != nil
        else { return false }
        linkClaimsVersion += 1
        return true
    }

    /// ADR-0079 — after a row was processed (or judged as this device's
    /// own): a commit that took effect moves the journal's epoch, and the
    /// record is kept at once (it only ever follows the journal's content).
    func observeJournalRow(
        _ mlsMessage: Data, messageId: Int64, group g: DeviceGroup,
        journal: inout FfiJournalEpochs, groupIdHex: String
    ) {
        guard let sealed = epochOfMessage(bytes: mlsMessage),
              let after = epochAfterMessage(bytes: mlsMessage), after > sealed,
              let groupEpoch = try? g.epoch(), let account = activeAccountIdB64 else { return }
        let next = journalEpochsObserve(
            epochs: journal, rowId: UInt64(max(0, messageId)), sealedEpoch: sealed,
            epochAfter: after, groupEpochAfter: groupEpoch)
        guard next != journal else { return }
        journal = next
        LinkClaims.Record(accountIdB64: account).setJournal(LinkClaims.Record.StoredJournal(
            groupIdHex: groupIdHex, base: next.base,
            commitRowIds: next.commitRowIds, commitEpochsAfter: next.commitEpochsAfter))
    }

    /// Wait until a fetch that STARTED after this call has found the
    /// DeviceGroup journal drained, so every row up to a claim just published
    /// has been applied. `false` when that did not happen in time.
    ///
    /// The same wait as ``waitForJournalHead(timeoutMs:)``, which cuts short a
    /// long poll already out and reads with wait 0 while a caller waits. This
    /// copy had neither and waited for the poll that was out to end on its
    /// own: on a simulator an invite-link answer took 37 s (2026-10-23).
    public func waitForJournalReadPastNow(
        timeoutMs: Int64 = DeviceGroupSyncRepository.journalHeadTimeoutMs
    ) async -> Bool {
        await waitForJournalHead(timeoutMs: timeoutMs)
    }

    /// ADR-0079 (reworked 2026-10-17) — another device's claim of a link
    /// request, taken in. Honoured only from a sender leaf the roster this
    /// row was sealed under resolves (C-68); its DS id is kept with that
    /// leaf's key. `true` when the envelope was a claim (kept or dropped).
    func receiveLinkClaim(_ envelope: Data, senderLeaf: UInt32?, group g: DeviceGroup, messageId: Int64) -> Bool {
        guard isLinkClaimEnvelope(bytes: envelope) else { return false }
        guard let claim = try? decodeLinkClaimEnvelope(plaintext: envelope) else { return true }
        guard let senderLeaf, let roster = try? g.devices(),
              let sender = roster.first(where: { $0.leafIndex == senderLeaf }),
              let account = activeAccountIdB64 else { return true }
        let changed = LinkClaims.Record(accountIdB64: account).recordSeen(
            LinkClaims.requestKeyHex(linkId: claim.linkId, requesterKey: claim.requesterAccountKey),
            messageId: messageId,
            authorHex: LinkClaims.hex(sender.signaturePublicKey),
            nowMs: Int64(Date().timeIntervalSince1970 * 1000))
        if changed { linkClaimsVersion += 1 }
        return true
    }

    // MARK: - Inbound poll loop

    /// When the last ADR-0073 supersession probe ran (unix ms), per process.
    private var lastSupersessionProbeMs: Int64 = 0
    /// At most one probe a minute while the journal keeps answering 401.
    static let supersessionProbeIntervalMs: Int64 = 60_000

    /// Ask the session token provider for a refresh, at most once a minute:
    /// a superseded generation fires its sign-out callback; anything else
    /// changes nothing here.
    private func probeSupersessionIfDue() async {
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        guard now - lastSupersessionProbeMs >= Self.supersessionProbeIntervalMs else { return }
        lastSupersessionProbeMs = now
        _ = await AntiCensorshipTransport.sessionTokens.refresh()
    }

    private func runPollLoop(accountIdB64: String, groupId: Data, startCursor: Int64 = 0) async {
        let ds = DsClient()
        var cursor: Int64 = startCursor
        // This loop reads ``identity`` and ``group`` afresh after every wait.
        // Once its binding has moved they are another account's (or the same
        // account's next session's): the batch in hand is then dropped and
        // the loop ends, whether or not the cancellation reached the request.
        let bound = binding.capture()
        // ADR-0073 R.5 — a lineage register whose send was never confirmed.
        await flushUnsentLineage()
        guard bound.isCurrent else { return }
        // ADR-0075 — the same for the recovery-email register.
        await flushUnsentRecoveryEmail()
        guard bound.isCurrent else { return }
        await flushUnsentOwnUsername()
        await flushUnsentQuickReaction()
        guard bound.isCurrent else { return }
        await flushUnsentLinkRecords()
        guard bound.isCurrent else { return }
        await flushUnsentDeviceAcks()
        while !Task.isCancelled, bound.isCurrent {
            if !pendingServiceEnvelopes.isEmpty {
                await flushPendingServiceEnvelopes()
                guard bound.isCurrent else { return }
            }
            // UX 17.6 — an acknowledgement whose send failed this session is
            // retried on every turn of the loop until it goes through.
            if deviceAcksUnsent {
                await flushUnsentDeviceAcks()
                guard bound.isCurrent else { return }
            }
            let result: FetchResult
            let fetchStartedAtMs = Int64(Date().timeIntervalSince1970 * 1000)
            // ADR-0053 point 1 — the read is signed under the DeviceGroup's
            // current epoch.
            let auth = group.flatMap { g in
                identity.flatMap {
                    DeviceGroupDsAccess.auth(group: g, identity: $0, accountIdB64: accountIdB64, ds: ds)
                }
            }
            let waitSecs = headWaiters > 0 ? 0 : 25
            let fetchTask = Task {
                try await ds.fetchGroup(
                    groupId: groupId,
                    after: cursor,
                    limit: nil,
                    waitSecs: waitSecs,
                    auth: auth
                )
            }
            pollFetchTask = fetchTask
            do {
                result = try await fetchTask.value
                pollFetchTask = nil
                pollKicked = false
            } catch where pollKicked {
                pollKicked = false
                pollFetchTask = nil
                continue
            } catch DsClientError.http(let status, _, _) where status == 404 {
                continue
            } catch let err as DsClientError {
                if case .network = err {
                    try? await Task.sleep(nanoseconds: 2_000_000_000)
                    continue
                }
                if case .http(401, _, _) = err {
                    // ADR-0073 section 5 — an attested reset on another device
                    // drops this journal's epoch chain, so the DeviceGroup
                    // read answers 401 at once while the session token is
                    // still within its TTL. The DS cannot say why; auth can:
                    // one throttled refresh learns `key-generation-superseded`
                    // and the provider's callback signs this device out
                    // (section 3.5) within a minute instead of at the access
                    // token's expiry.
                    await probeSupersessionIfDue()
                }
                NSLog("DeviceGroupSync poll fetch failed %@", String(describing: err))
                try? await Task.sleep(nanoseconds: 5_000_000_000)
                continue
            } catch {
                NSLog("DeviceGroupSync poll fetch unexpected %@", error.localizedDescription)
                try? await Task.sleep(nanoseconds: 5_000_000_000)
                continue
            }

            // ADR-0058 / UX spec chat-folders section 6 — the fetch went
            // through, so the network is back: whatever failed to publish
            // earlier goes out now, as the whole state.
            guard bound.isCurrent else { return }
            await flushPending()

            // ADR-0081 decision 2, step 6 — a DeviceGroup commit of this device
            // that did not finish (the DS could not be reached, a write failed,
            // a crash) is finished before anything else; until it is, this
            // device never reads itself as behind (the DS may hold its own next
            // epoch). A landed entry made its epoch current and reloaded the
            // handle (``reloadFromDisk(accountIdB64:)``).
            var commitPending = false
            if let accountId = activeAccountId {
                do {
                    if let owed = try await DeviceGroupCommits.resendOwed(
                        accountIdBytes: accountId, accountIdB64: accountIdB64) {
                        NSLog("DeviceGroup commit outbox: %@", owed == .lost ? "lost" : "landed")
                    }
                } catch {
                    commitPending = true
                    NSLog("DeviceGroup commit outbox: not finished yet (%@)", String(describing: error))
                }
            }
            guard bound.isCurrent else { return }

            // G-13 (ADR-0015): the 32-byte verifying key peers feed to
            // processIncomingVerifyingRemovals. A zero key is a fail-closed
            // fallback when the device-attest endpoint is unreachable — it
            // parses but verifies nothing, so a removal the policy requires to
            // be attested is refused rather than applied unverified; while the
            // policy is off, entries apply normally regardless.
            // Without the key the batch is HELD, not applied under a zero key:
            // a zero key refuses a removal the policy requires to be attested,
            // and a refused row is skipped for good once the cursor moves past
            // it. A device removed that way would stay an epoch behind, drain
            // the journal, read as forked and re-found the DeviceGroup over
            // every healthy device (ADR-0057, 2026-09-30).
            let attestVk: Data
            do {
                attestVk = try await DeviceAttestKey.fetch()
            } catch {
                NSLog("DeviceGroupSync device-attest key fetch failed; holding the batch: %@", error.localizedDescription)
                try? await Task.sleep(nanoseconds: 5_000_000_000)
                continue
            }
            guard bound.isCurrent else { return }

            var mutated = false
            // ADR-0079 (order of 2026-10-17) — the journal readability rule for
            // this batch: own claims and notes judged at their rows, commits
            // moving the journal's epoch.
            var journal = group.flatMap { loadJournalEpochs($0) }
            for message in result.messages {
                cursor = message.id
                guard let id = identity, let g = group else { return }
                // Unwrap the journal envelope (MLS message + any attestation).
                let entry: (message: Data, attestation: Data?)
                do {
                    entry = try DeviceGroupJournalEnvelope.decode(message.payload)
                } catch {
                    NSLog("DeviceGroupSync envelope decode failed: %@", error.localizedDescription)
                    continue
                }
                // ADR-0079 — a commit that took effect moves the journal's
                // epoch; observed after the row, whatever became of it.
                defer {
                    if var j = journal {
                        observeJournalRow(
                            entry.message, messageId: message.id, group: g,
                            journal: &j.epochs, groupIdHex: j.groupIdHex)
                        journal = j
                    }
                }
                // ADR-0079 — this device's own claim or note, recognised by
                // the sealed bytes it recorded before the publish and judged
                // at this row's position; recorded before the cursor moves
                // past it.
                if let j = journal, judgeOwnLinkRow(entry.message, messageId: message.id, journal: j.epochs) {
                    continue
                }
                let attestations: [Data] = entry.attestation.map { [$0] } ?? []
                // ADR-0058 section 5.1 — the members before this row, to tell
                // whether a commit grew the group.
                let devicesBefore = (try? g.devices()) ?? []
                let processed: ProcessedMessage
                do {
                    processed = try g.processIncomingVerifyingRemovals(
                        identity: id,
                        messageBytes: entry.message,
                        attestations: attestations,
                        attestVerifyingKey: attestVk,
                        nowUnixSecs: Int64(Date().timeIntervalSince1970)
                    )
                } catch {
                    NSLog("DeviceGroupSync processIncomingVerifyingRemovals failed: %@", error.localizedDescription)
                    continue
                }
                mutated = true
                switch processed {
                case .application(let data, let senderLeaf, _):
                    // ADR-0023 added the sender leaf to the
                    // Application case. Removals do not authorise on it
                    // (they are gated upstream by the attestation verify);
                    // ADR-0080's device card keys on it, resolved to the
                    // leaf's signature key HERE, against the roster this row
                    // was sealed under (C-68: a leaf index is never stored).
                    do {
                        let parsed = try parseDevicegroupPayload(plaintext: data)
                        if case .serviceEnvelope(let envelope) = parsed.body,
                           receiveDeviceCard(envelope, senderLeaf: senderLeaf, group: g) {
                            break
                        }
                        // ADR-0079 (reworked 2026-10-17) — another device's
                        // claim of a link request, resolved the same way.
                        if case .serviceEnvelope(let envelope) = parsed.body,
                           receiveLinkClaim(envelope, senderLeaf: senderLeaf, group: g, messageId: message.id) {
                            break
                        }
                        // ADR-0079 (order of 2026-10-17) — another device's
                        // link-founded note, resolved the same way.
                        if case .serviceEnvelope(let envelope) = parsed.body,
                           receiveLinkFounded(envelope, senderLeaf: senderLeaf, group: g, messageId: message.id) {
                            break
                        }
                        applyParsed(parsed)
                    } catch {
                        NSLog("DeviceGroupSync parseDevicegroupPayload failed: %@", error.localizedDescription)
                    }
                case .commit:
                    noteMembershipChange(before: devicesBefore, group: g)
                    rosterChange.commitApplied()
                    // ADR-0081 decision 4 — the sibling's record of which keys
                    // it saw added, and when (the row's DS time), so a knock
                    // answered by another sibling is not answered twice.
                    if let record = admissions, let after = try? g.devices() {
                        let beforeKeys = Set(devicesBefore.map(\.signaturePublicKey))
                        let at = Self.parseMs(message.createdAt) ?? Int64(Date().timeIntervalSince1970 * 1000)
                        admissions = after
                            .filter { !beforeKeys.contains($0.signaturePublicKey) }
                            .reduce(record) { $0.noteAdded(keyHex: hexEncode($1.signaturePublicKey), atMs: at) }
                    }
                case .proposal, .externalJoinProposal:
                    // No application-state mutation.
                    break
                }
            }

            if mutated, let id = identity {
                // ADR-0081 decision 3 — fail-closed and conditional on the
                // generation this handle read: a handle another one wrote past
                // (a commit of this device made N+1 current) reloads instead of
                // putting its older state back. Any other failure keeps the
                // applied state in memory; the next batch writes it again (the
                // cursor is in memory too, so nothing reads past what this
                // process holds).
                do {
                    handleGeneration = try DeviceGroupLifecycle.persistSnapshotOrThrow(
                        identity: id, accountIdB64: accountIdB64, expectedGeneration: handleGeneration)
                    saveAdmissions(accountIdB64: accountIdB64)
                    if rosterChange.persisted() { DeviceRosterChanges.note(accountIdB64: accountIdB64) }
                } catch is DeviceGroupSnapshotStore.SnapshotChanged {
                    NSLog("DeviceGroupSync: the snapshot moved under this handle; reloading")
                    reloadFromDisk(accountIdB64: accountIdB64)
                    if rosterChange.persisted() { DeviceRosterChanges.note(accountIdB64: accountIdB64) }
                } catch {
                    NSLog("DeviceGroupSync snapshot persist failed: %@", String(describing: error))
                }
                // ADR-0057 — no heal-on-apply: the device that committed
                // registered the epoch, with its commit (ADR-0081).
            }

            // A row in this batch removed THIS device (docs/ux/second-device.md
            // section 3.1). The batch that carried the removal was persisted
            // above, so the next launch finds the inactive snapshot and shows
            // the same screen; nothing after it is polled or persisted.
            if !Task.isCancelled, let g = group,
               noteDeviceGroupActivity(RemovedDevice.readActive { try g.isActive() }) {
                return
            }

            let drained = result.messages.isEmpty && !result.hasMore
            if drained {
                lastDrainedFetchAtMs = Int64(Date().timeIntervalSince1970 * 1000)
                lastDrainedFetchStartedAtMs = fetchStartedAtMs
            }
            // ADR-0081 decision 4 — behind for good? A drained read (no rows,
            // nothing more) while the DS names a newer epoch than this
            // handle's: since an epoch is registered only with its commit row,
            // nothing left in the journal can bring this device forward. It
            // drops its DeviceGroup state and rejoins through a sibling.
            if drained, let g = group, let id = identity,
               let localEpoch = try? g.dsAccessSigner(identity: id).epoch(),
               DeviceGroupRejoin.isBehind(
                   localEpoch: localEpoch, drained: drained, latestEpoch: result.latestEpoch,
                   commitPending: commitPending),
               RemovedDevice.rejoinAllowed(RemovedDevice.readActive({ try g.isActive() })) {
                NSLog("DeviceGroupSync: behind the DS with nothing left to read (local epoch %llu, DS epoch %llu); rejoining",
                      localEpoch, result.latestEpoch ?? 0)
                enterRejoin(accountIdB64: accountIdB64)
                return
            }
            // At the head: a sibling reads the knocks (ADR-0081 decision 4).
            let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
            if drained, !commitPending, nowMs - lastKnockCheckAtMs >= DeviceGroupRejoin.knockCheckIntervalMs {
                lastKnockCheckAtMs = nowMs
                do {
                    try await checkKnocks(accountIdB64: accountIdB64, bound: bound)
                } catch {
                    NSLog("DeviceGroupSync knock check failed: %@", String(describing: error))
                }
                guard bound.isCurrent else { return }
            }
        }
    }

    // MARK: - ADR-0081 decision 4: founding, rejoining, admitting

    /// No usable DeviceGroup state at bootstrap. Ask the directory: with no
    /// other own device listed, found (nobody can be cut off); otherwise
    /// rejoin through a sibling. A directory that cannot be read founds
    /// nothing; the question is asked again.
    private func startFoundOrRejoin(accountIdBytes: Data, accountIdB64: String, deviceSeed: Data) {
        guard rejoinTask == nil else { return }
        let bound = binding.capture()
        rejoinTask = Task { [weak self] in
            var mayFound: Bool?
            while mayFound == nil, !Task.isCancelled, bound.isCurrent {
                do {
                    mayFound = try await Self.directoryListsNoOtherDevice(
                        accountIdBytes: accountIdBytes, deviceSeed: deviceSeed)
                } catch {
                    NSLog("DeviceGroupSync: the directory could not be read; founding nothing (%@)",
                          String(describing: error))
                    try? await Task.sleep(nanoseconds: 5_000_000_000)
                }
            }
            guard let self, let mayFound, !Task.isCancelled, bound.isCurrent,
                  self.activeAccountIdB64 == accountIdB64 else { return }
            self.otherDeviceSignedIn = !mayFound
            if mayFound {
                do {
                    let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
                        accountIdBytes: accountIdBytes, accountIdB64: accountIdB64,
                        deviceIdentitySeed: deviceSeed, found: true)
                    self.rejoinTask = nil
                    NSLog("DeviceGroupSync: no other device listed; DeviceGroup founded")
                    self.adopt(ensured, accountIdB64: accountIdB64)
                } catch {
                    NSLog("DeviceGroupSync: founding failed; retried next session (%@)",
                          String(describing: error))
                    self.rejoinTask = nil
                }
                return
            }
            NSLog("DeviceGroupSync: no DeviceGroup state while another device is listed; rejoining")
            await self.runRejoin(accountIdBytes: accountIdBytes, accountIdB64: accountIdB64, bound: bound)
        }
    }

    /// Stop following the DeviceGroup, drop its local state, and rejoin.
    /// Called from inside the poll loop, which returns right after.
    private func enterRejoin(accountIdB64: String) {
        guard let accountId = activeAccountId, activeAccountIdB64 == accountIdB64,
              !deviceRemoved, rejoinTask == nil else { return }
        resendTask?.cancel()
        resendTask = nil
        pollTask = nil
        identity = nil
        group = nil
        handleGeneration = nil
        admissions = nil
        ready = false
        let bound = binding.capture()
        rejoinTask = Task { [weak self] in
            await self?.runRejoin(accountIdBytes: accountId, accountIdB64: accountIdB64, bound: bound)
        }
    }

    /// The device's side (``DeviceGroupRejoin/rejoinOnce(ports:firstPass:)``),
    /// until joined or the session ends; then the joined DeviceGroup is loaded
    /// like a bootstrap would.
    private func runRejoin(accountIdBytes: Data, accountIdB64: String, bound: BindingEpoch.Captured) async {
        let ports = rejoinPorts(accountIdBytes: accountIdBytes, accountIdB64: accountIdB64)
        var first = true
        while !Task.isCancelled, bound.isCurrent {
            do {
                let outcome = try await DeviceGroupRejoin.rejoinOnce(ports: ports, firstPass: first)
                first = false
                if outcome == .joined { break }
            } catch {
                NSLog("DeviceGroupSync rejoin attempt failed; trying again (%@)", String(describing: error))
                try? await Task.sleep(nanoseconds: 5_000_000_000)
            }
        }
        guard !Task.isCancelled, bound.isCurrent, activeAccountIdB64 == accountIdB64 else { return }
        rejoinTask = nil
        do {
            let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(accountIdB64: accountIdB64)
            let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
                accountIdBytes: accountIdBytes, accountIdB64: accountIdB64, deviceIdentitySeed: seed)
            NSLog("DeviceGroupSync: rejoined the account's DeviceGroup")
            adopt(ensured, accountIdB64: accountIdB64)
        } catch {
            NSLog("DeviceGroupSync: the joined DeviceGroup did not load (%@)", String(describing: error))
        }
    }

    private func rejoinPorts(accountIdBytes: Data, accountIdB64: String) -> DeviceGroupRejoinPorts {
        let store = DeviceGroupSnapshotStore()
        return DeviceGroupRejoinPorts(
            dropLocalState: {
                store.dropStateForRejoin(accountIdB64: accountIdB64)
            },
            publishDirectoryKeyPackage: { [weak self] in
                try await self?.republishDirectoryKeyPackage()
            },
            loadState: {
                store.getPending(accountIdB64: accountIdB64, kind: .rejoin)
                    .flatMap(DeviceGroupRejoinState.decode)
            },
            saveState: { state in
                try store.putPending(accountIdB64: accountIdB64, kind: .rejoin, blob: try state.encoded())
            },
            knock: { [weak self] nowMs in
                guard let loginHandle = self?.loginHandleProvider() else {
                    throw DeviceGroupSyncError.notReady
                }
                guard let groupId = DeviceGroupLifecycle.groupId(accountIdB64: accountIdB64) else {
                    throw DeviceGroupLifecycle.NoAccountSeed()
                }
                let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(accountIdB64: accountIdB64)
                let joiner = try Identity.fromSeed(seed: seed)
                let keyPackage = try joiner.createKeyPackage()
                let rendezvous = PairingCodec.generateRendezvousPseudonym()
                // ADR-0081 amendment — the device identity's key IS this
                // device's old DeviceGroup leaf key: it signs that its leaf may
                // be replaced by this KeyPackage in the commit leaving the
                // journal's latest epoch (an empty knock is a probe that names
                // it), which lets a sibling readmit it under the
                // attested-removal policy without a passkey. An unclaimed
                // journal names no epoch: no proof.
                let ds = DsClient()
                let probe = try await ds.knock(groupId: groupId, payloads: [])
                let payload: Data
                if let epoch = probe.epoch {
                    let proof = try signDeviceGroupSelfRemoval(
                        oldIdentity: joiner, groupId: groupId, epoch: epoch, replacementKeyPackage: keyPackage)
                    payload = try PairingCodec.buildRejoinKnockPayload(
                        loginHandle: loginHandle, keyPackage: keyPackage, rendezvousPseudonym: rendezvous,
                        selfRemovalProof: proof)
                } else {
                    payload = try PairingCodec.buildPairingQrPayload(
                        loginHandle: loginHandle, keyPackage: keyPackage, rendezvousPseudonym: rendezvous)
                }
                let state = DeviceGroupRejoinState(
                    rendezvousB64: base64Encode(rendezvous),
                    joinerSnapshotB64: base64Encode(try joiner.snapshotStorage()),
                    knockedAtMs: nowMs)
                // Kept BEFORE the knock: the joiner's provider is the only
                // thing that can open the Welcome a sibling sends.
                try store.putPending(accountIdB64: accountIdB64, kind: .rejoin, blob: try state.encoded())
                _ = try await ds.knock(groupId: groupId, payloads: [payload])
                return state
            },
            awaitWelcome: { state, deadlineMs in
                guard let rendezvous = Data(base64Encoded: state.rendezvousB64) else { return nil }
                let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
                let remaining = max(1, min(deadlineMs - nowMs, DeviceGroupRejoin.reknockAfterMs))
                let outcome = try await WelcomeCourier.pollForWelcome(
                    rendezvousPseudonym: rendezvous, timeoutSeconds: TimeInterval(remaining) / 1000)
                if case .received(let welcome, _) = outcome { return welcome }
                return nil
            },
            join: { state, welcome in
                guard let joinerSnapshot = Data(base64Encoded: state.joinerSnapshotB64) else {
                    throw DeviceGroupSyncError.notReady
                }
                let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(accountIdB64: accountIdB64)
                let joiner = try Identity.fromSeed(seed: seed)
                DeviceGroupLifecycle.noteHandle(joiner, accountIdB64: accountIdB64)
                try joiner.restoreStorage(snapshot: joinerSnapshot)
                _ = try DeviceGroup.joinFromWelcome(
                    welcome: welcome, identity: joiner, accountId: accountIdBytes)
                try DeviceGroupLifecycle.persistSnapshotOrThrow(identity: joiner, accountIdB64: accountIdB64)
                // A fresh observer of the journal.
                try store.putPending(
                    accountIdB64: accountIdB64, kind: .admissions,
                    blob: try DeviceGroupAdmissionRecord(
                        watchingSinceMs: Int64(Date().timeIntervalSince1970 * 1000)).encoded())
            },
            clearState: {
                store.deletePending(accountIdB64: accountIdB64, kind: .rejoin)
            },
            now: { Int64(Date().timeIntervalSince1970 * 1000) }
        )
    }

    /// The directory check of the founding policy: `true` when the account's
    /// directory lists no own device other than this one. A failure throws
    /// (nothing is founded on an unread directory).
    private static func directoryListsNoOtherDevice(accountIdBytes: Data, deviceSeed: Data) async throws -> Bool {
        let ownKey = try Identity.fromSeed(seed: deviceSeed).signaturePublicKey()
        let listed = try await DiscoveryClient().fetchKeyPackages(accountId: accountIdBytes) ?? []
        return listed.allSatisfy { $0.deviceId == ownKey }
    }

    /// The account's directory rows whose device binding verifies under the
    /// account key the conversation leaves are bound to: leaf key to its
    /// directory KeyPackage. A deleted device is tombstoned (ADR-0074) and not
    /// listed.
    private static func ownDirectory(accountIdBytes: Data, accountIdB64: String) async throws -> [Data: Data] {
        guard let lineage = DeviceGroupLifecycle.lineageSeed(accountIdB64: accountIdB64) else { return [:] }
        let accountKey = try Identity.fromSeed(seed: lineage).signaturePublicKey()
        let listed = try await DiscoveryClient().fetchKeyPackages(accountId: accountIdBytes) ?? []
        var out: [Data: Data] = [:]
        for row in listed {
            guard let bound = (try? keyPackageAccountKey(keyPackage: row.keyPackage)) ?? nil else { continue }
            // A Phase 1 unbound leaf resolves to itself: not a binding.
            guard bound == accountKey, bound != row.deviceId else { continue }
            out[row.deviceId] = row.keyPackage
        }
        return out
    }

    private func loadAdmissions(accountIdB64: String) {
        admissions = DeviceGroupSnapshotStore()
            .getPending(accountIdB64: accountIdB64, kind: .admissions)
            .flatMap(DeviceGroupAdmissionRecord.decode)
            ?? DeviceGroupAdmissionRecord(watchingSinceMs: Int64(Date().timeIntervalSince1970 * 1000))
    }

    private func saveAdmissions(accountIdB64: String) {
        guard let admissions, activeAccountIdB64 == accountIdB64 else { return }
        do {
            try DeviceGroupSnapshotStore().putPending(
                accountIdB64: accountIdB64, kind: .admissions, blob: try admissions.encoded())
        } catch {
            // The record is a guard against admitting twice; an unsaved update
            // is rebuilt from the journal by the next batch.
            NSLog("DeviceGroup admission record not saved: %@", String(describing: error))
        }
    }

    /// A sibling's side: read the DeviceGroup's knocks and admit each one
    /// ``DeviceGroupRejoin/admissionVerdict(_:)`` allows — replace the
    /// device's previous leaf, if it is still in the roster, by its KeyPackage
    /// in one commit carrying its self-removal proof (ADR-0081 amendment),
    /// else add the KeyPackage; one commit through the outbox, the Welcome to
    /// the knock's rendezvous.
    private func checkKnocks(accountIdB64: String, bound: BindingEpoch.Captured) async throws {
        guard let g = group, let id = identity, let accountId = activeAccountId, admissions != nil,
              let loginHandle = loginHandleProvider() else { return }
        let ds = DsClient()
        let knocks = try await ds.fetchKnocks(
            groupId: try g.groupId(), after: knockCursor,
            auth: DeviceGroupDsAccess.auth(group: g, identity: id, accountIdB64: accountIdB64, ds: ds))
        guard bound.isCurrent, !knocks.messages.isEmpty else { return }
        knockCursor = knocks.messages.last?.id ?? knockCursor
        guard let held = group, let heldId = identity else { return }
        let roster = ((try? held.devices()) ?? []).map {
            DeviceGroupRejoin.RosterMember(leaf: $0.leafIndex, key: $0.signaturePublicKey)
        }
        let ownKey = heldId.signaturePublicKey()
        let policy = (try? held.requireAttestedRemovals()) ?? false
        let heldGroupId = try held.groupId()
        let heldEpoch = try held.epoch()
        var directory: [Data: Data]?
        for knock in knocks.messages {
            guard let decoded = try? PairingCodec.parsePairingQrPayload(
                      wireBytes: knock.payload, expectedLoginHandle: loginHandle),
                  let key = try? keyPackageSignatureKey(keyPackage: decoded.keyPackage)
            else { continue }
            if directory == nil {
                directory = try await Self.ownDirectory(accountIdBytes: accountId, accountIdB64: accountIdB64)
                guard bound.isCurrent else { return }
            }
            guard let record = admissions else { return }
            let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
            // Checked BEFORE a commit is built: a commit the DS takes but every
            // peer refuses would leave this device ahead of them.
            let proof = decoded.selfRemovalProof
            let proofCovers = proof.map {
                deviceGroupSelfRemovalCovers(
                    identity: heldId, proof: $0, groupId: heldGroupId, epoch: heldEpoch,
                    removedSignaturePubkey: key, keyPackage: decoded.keyPackage)
            } ?? false
            let verdict = DeviceGroupRejoin.admissionVerdict(DeviceGroupRejoin.AdmissionInput(
                knockId: knock.id,
                knockAtMs: Self.parseMs(knock.createdAt) ?? 0,
                key: key,
                ownKey: ownKey,
                roster: roster,
                handled: Set(record.handled),
                watchingSinceMs: record.watchingSinceMs,
                lastAddedAtMs: record.lastAddedAtMs[hexEncode(key)],
                listed: directory?[key] != nil,
                policyRequiresAttestation: policy,
                selfRemovalProofCovers: proofCovers,
                keyPackageAlreadyIn: (try? held.containsLeafOfKeyPackage(
                    identity: heldId, keyPackage: decoded.keyPackage)) ?? false,
                nowMs: nowMs))
            guard verdict == .admit else { continue }
            let added = try await DeviceGroupCommits.commitChange(
                accountIdBytes: accountId,
                accountIdB64: accountIdB64,
                build: { fresh in
                    // Another sibling answered it meanwhile.
                    if (try? fresh.group.containsLeafOfKeyPackage(
                        identity: fresh.identity, keyPackage: decoded.keyPackage)) ?? false {
                        return nil
                    }
                    let previous = ((try? fresh.group.devices()) ?? [])
                        .first(where: { $0.signaturePublicKey == key })
                    guard let previous else {
                        let addition = try fresh.group.addDeviceFromWire(
                            identity: fresh.identity, keyPackage: decoded.keyPackage)
                        return DeviceGroupCommits.Built(
                            kind: .rejoinAdd,
                            row: DeviceGroupJournalEnvelope.encode(message: addition.commit, attestation: nil),
                            welcome: addition.welcome,
                            welcomeRendezvous: decoded.rendezvousPseudonym)
                    }
                    // ADR-0081 amendment — the previous leaf out and the new
                    // one in, in ONE commit; the proof rides in the
                    // removal-authorisation slot. Re-checked at the epoch this
                    // commit leaves: a proof names one epoch, and peers refuse
                    // it at any other. Under the policy a removal it does not
                    // cover would be refused by every peer.
                    let freshEpoch = try fresh.group.epoch()
                    let freshGroupId = try fresh.group.groupId()
                    let covers = proof.map {
                        deviceGroupSelfRemovalCovers(
                            identity: fresh.identity, proof: $0, groupId: freshGroupId, epoch: freshEpoch,
                            removedSignaturePubkey: key, keyPackage: decoded.keyPackage)
                    } ?? false
                    if !covers, (try? fresh.group.requireAttestedRemovals()) ?? true { return nil }
                    let replaced = try fresh.group.replaceDeviceAt(
                        identity: fresh.identity, leafIndex: previous.leafIndex,
                        keyPackage: decoded.keyPackage)
                    return DeviceGroupCommits.Built(
                        kind: .rejoinReplace,
                        row: DeviceGroupJournalEnvelope.encode(
                            message: replaced.commit, attestation: covers ? proof : nil),
                        welcome: replaced.welcome,
                        welcomeRendezvous: decoded.rendezvousPseudonym)
                },
                after: { _ in () }
            )
            guard bound.isCurrent else { return }
            if let added, case .landed = added.outcome, let current = admissions {
                admissions = current.noteAdded(keyHex: hexEncode(key), atMs: nowMs).noteHandled(knock.id)
                saveAdmissions(accountIdB64: accountIdB64)
                NSLog("DeviceGroupSync: a device of this account was admitted again")
            }
            // The commits reloaded the long-lived handle; the next pass re-reads.
            return
        }
    }

    /// A DS `created_at` (ISO 8601, with or without fractional seconds) as
    /// unix milliseconds.
    static func parseMs(_ iso: String) -> Int64? {
        let withFraction = ISO8601DateFormatter()
        withFraction.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = withFraction.date(from: iso) { return Int64(date.timeIntervalSince1970 * 1000) }
        let plain = ISO8601DateFormatter()
        plain.formatOptions = [.withInternetDateTime]
        return plain.date(from: iso).map { Int64($0.timeIntervalSince1970 * 1000) }
    }

    /// ADR-0081 — reload the held handle from the snapshot after another
    /// handle on this device made a new epoch current (the commit outbox), or
    /// after a write of this handle was refused because one had.
    private func reloadFromDisk(accountIdB64: String) {
        guard activeAccountIdB64 == accountIdB64, !deviceRemoved, group != nil,
              let accountId = activeAccountId else { return }
        let before = (try? group?.devices()) ?? []
        do {
            let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(accountIdB64: accountIdB64)
            let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
                accountIdBytes: accountId, accountIdB64: accountIdB64, deviceIdentitySeed: seed)
            identity = ensured.identity
            group = ensured.group
            handleGeneration = ensured.generation
            if noteDeviceGroupActivity(RemovedDevice.readActive { try ensured.group.isActive() }) {
                return
            }
            noteMembershipChange(before: before, group: ensured.group)
            sendHandOverIfGrown(before: before, group: ensured.group, accountIdB64: accountIdB64)
        } catch {
            NSLog("DeviceGroupSync reload failed: %@", String(describing: error))
        }
    }

    /// ADR-0060 section 7 — a device was added: send it the setting, the
    /// reach-key versions and (ADR-0048 section 4a) the own profile, original
    /// stamps, through THIS handle, reloaded at the post-add epoch. Never from
    /// the committing handle: two handles of one DeviceGroup sealing at the
    /// same sender generation reuse a key and nonce (seen live on web
    /// 2026-09-30, SecretReuseError on the new device).
    private func sendHandOverIfGrown(before: [DeviceLeaf], group g: DeviceGroup, accountIdB64: String) {
        let after = (try? g.devices()) ?? []
        guard after.count > before.count else { return }
        let envelopes = Self.handOverEnvelopes(
            accountIdB64: accountIdB64, reach: .shared, profiles: .shared)
            + [DeviceCardCenter.shared.ownEnvelope()].compactMap { $0 }
        if DeviceCardCenter.shared.ownEnvelope() != nil {
            NSLog("device card: own card queued to send to a newly added device")
        }
        Task { @MainActor [weak self] in
            for envelope in envelopes {
                await self?.sendServiceEnvelope(envelope)
            }
        }
    }

    /// Fold one payload another device sent. Every kind goes through the one
    /// fold, the ADR-0058 catch-up parts included; a payload that changes
    /// nothing (older, or already held) is a no-op.
    func applyParsed(_ parsed: ParsedDeviceGroupPayload) {
        if case .serviceEnvelope(let envelope) = parsed.body {
            // ADR-0073 R.5 / R.13a — a lineage envelope is routed BEFORE the
            // lakko-contacts dispatcher: it is a `lakko-sync` register,
            // folded by the one fold. A malformed register is dropped.
            if isLineageEnvelope(bytes: envelope) {
                do {
                    if try syncedState.applyLineageEnvelope(envelope: envelope) {
                        if let maxHlc = try? syncedState.maxHlc() { Hlc.observe(maxHlc) }
                        stateChanged()
                    }
                } catch {
                    NSLog("ADR-0073: lineage envelope refused: %@", error.localizedDescription)
                }
                return
            }
            // ADR-0075 — the recovery-email register, routed before the
            // lakko-contacts dispatcher for the same reason.
            if isRecoveryEmailEnvelope(bytes: envelope) {
                do {
                    if try syncedState.applyRecoveryEmailEnvelope(envelope: envelope) {
                        if let maxHlc = try? syncedState.maxHlc() { Hlc.observe(maxHlc) }
                        stateChanged()
                    }
                } catch {
                    NSLog("ADR-0075: recovery-email envelope refused")
                }
                return
            }
            // ADR-0079 — a link-record register, the same way.
            if isLinkRecordEnvelope(bytes: envelope) {
                do {
                    if try syncedState.applyLinkRecordEnvelope(envelope: envelope) {
                        if let maxHlc = try? syncedState.maxHlc() { Hlc.observe(maxHlc) }
                        stateChanged()
                    }
                } catch {
                    NSLog("ADR-0079: link-record envelope refused")
                }
                return
            }
            // UX 17.6 — a device acknowledgement, the same way: the synced
            // acknowledged set reaches the new-device warning through
            // ``refreshDerived()``.
            if isDeviceAckEnvelope(bytes: envelope) {
                do {
                    if try syncedState.applyDeviceAckEnvelope(envelope: envelope) {
                        if let maxHlc = try? syncedState.maxHlc() { Hlc.observe(maxHlc) }
                        stateChanged()
                    }
                } catch {
                    NSLog("device-ack: envelope refused")
                }
                return
            }
            // Reactions-and-emoji spec section 6 (D3) — the quick reaction,
            // the same way.
            if isQuickReactionEnvelope(bytes: envelope) {
                do {
                    if try syncedState.applyQuickReactionEnvelope(envelope: envelope) {
                        if let maxHlc = try? syncedState.maxHlc() { Hlc.observe(maxHlc) }
                        stateChanged()
                    }
                } catch {
                    NSLog("quick-reaction: envelope refused")
                }
                return
            }
            // ADR-0076 — the own-username register, the same way.
            if isOwnUsernameEnvelope(bytes: envelope) {
                do {
                    if try syncedState.applyOwnUsernameEnvelope(envelope: envelope) {
                        if let maxHlc = try? syncedState.maxHlc() { Hlc.observe(maxHlc) }
                        stateChanged()
                    }
                } catch {
                    NSLog("ADR-0076: own-username envelope refused")
                }
                return
            }
            // ADR-0074 section 3.3 — the account's own `key-rotation`
            // envelope: meaningful here and nowhere else. It never reaches
            // the reach / profile adopters.
            if Self.isKeyRotation(envelope) {
                onKeyRotationEnvelope(envelope)
                return
            }
            // ADR-0060 section 7 — the Visibility setting and the ReachState
            // counters, ordered by the envelope's own stamp, not by the
            // DeviceGroup HLC: a setting is kept only when strictly newer,
            // counters are merged by the core; any other kind is dropped.
            onServiceEnvelope(envelope)
            return
        }
        applyReceived(parsed)
    }

    private func applyReceived(_ parsed: ParsedDeviceGroupPayload) {
        do {
            if try syncedState.applyPayload(payload: parsed) {
                if let maxHlc = try? syncedState.maxHlc() { Hlc.observe(maxHlc) }
                stateChanged()
            }
        } catch {
            NSLog("DeviceGroupSync fold refused a payload: %@", error.localizedDescription)
        }
    }

    // MARK: - Growth re-send (ADR-0058 section 5.1)

    /// A commit changed the DeviceGroup (received here, or made on this
    /// device through its own handle). When it added a device and this is
    /// the lowest leaf among the members before, re-send the whole folded
    /// state after a random wait of up to 30 seconds.
    private func noteMembershipChange(before: [DeviceLeaf], group g: DeviceGroup) {
        reportRoster(group: g)
        guard let after = try? g.devices(), let own = try? g.ownLeafIndex() else { return }
        // ADR-0080 — every device that was there re-sends its own card.
        let beforeKeys = Set(before.map(\.signaturePublicKey))
        if after.contains(where: { !beforeKeys.contains($0.signaturePublicKey) }) {
            DeviceCardCenter.shared.scheduleResend()
        }
        guard DeviceGroupGrowth.shouldResend(ownLeaf: own, before: before, after: after) else {
            return
        }
        if resendTask != nil { return }
        resendTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: DeviceGroupGrowth.randomDelayNanoseconds())
            guard !Task.isCancelled else { return }
            await self?.sendSnapshot()
            self?.resendTask = nil
        }
    }

    /// ADR-0074 section 2.3 — the leaf keys of the account's DeviceGroup as
    /// the live handle reads them: the account's own, MLS-authenticated
    /// record of its devices, which the conversations' own-device admission
    /// is filtered by. `nil` when no handle is held, this device was removed,
    /// or the roster cannot be read (never an empty set standing for "no
    /// devices").
    public func rosterDeviceKeys() -> [Data]? {
        guard !deviceRemoved, let devices = try? group?.devices(), !devices.isEmpty else { return nil }
        return devices.map(\.signaturePublicKey)
    }

    /// UX 17.5 — hand the account's device roster, as this handle reads it,
    /// to the new-device notice: a device this one has not acknowledged
    /// raises the banner. A roster that cannot be read reports nothing
    /// (never "every device left").
    private func reportRoster(group g: DeviceGroup) {
        guard let accountIdB64 = activeAccountIdB64, !deviceRemoved,
              let devices = try? g.devices(), !devices.isEmpty else { return }
        otherDeviceSignedIn = devices.count > 1
        let own = (try? g.ownLeafIndex()).flatMap { leaf in
            devices.first { $0.leafIndex == leaf }?.signaturePublicKey
        }
        NewDeviceNoticeCenter.shared.observe(
            accountIdB64: accountIdB64,
            roster: devices.map(\.signaturePublicKey),
            ownKey: own)
        // ADR-0080 — a leaf that left takes its card with it.
        DeviceCardCenter.shared.prune(rosterKeysHex: devices.map { hexEncode($0.signaturePublicKey) })
    }

    /// ADR-0080 — take in `envelope` when it is a device card. `true` when it
    /// was one (kept or dropped), `false` for any other envelope. The sender
    /// is the MLS-authenticated leaf of the row, resolved against the roster
    /// at receipt; a row with no sender leaf, or a leaf the roster does not
    /// hold, is dropped.
    func receiveDeviceCard(_ envelope: Data, senderLeaf: UInt32?, group g: DeviceGroup) -> Bool {
        guard let card = try? decodeDeviceCardEnvelope(plaintext: envelope) else { return false }
        guard let senderLeaf, let roster = try? g.devices(),
              let sender = roster.first(where: { $0.leafIndex == senderLeaf }) else {
            NSLog("device card: received a card from no device of the roster and refused it")
            return true
        }
        DeviceCardCenter.shared.receive(
            senderKeyHex: hexEncode(sender.signaturePublicKey),
            card: DeviceCardCenter.stored(card),
            rosterKeysHex: roster.map { hexEncode($0.signaturePublicKey) })
        return true
    }

    /// Publish every catch-up part as its own DeviceGroup message, each under
    /// a fresh envelope stamp; the registers inside keep their original
    /// stamps. Not folded here: it is this device's own state already. A part
    /// that cannot be sent marks the state pending, so the next successful
    /// poll re-sends it.
    private func sendSnapshot() async {
        if deviceRemoved { return }
        let parts: [SyncedSnapshotPart]
        do {
            parts = try syncedState.snapshotParts()
        } catch {
            NSLog("DeviceGroupSync snapshot build failed: %@", error.localizedDescription)
            return
        }
        for part in parts {
            do {
                try await sendBody(.stateSnapshot(part: part), hlc: Hlc.mint())
            } catch {
                NSLog("DeviceGroupSync snapshot part %u/%u not sent: %@",
                      part.part() + 1, part.parts(), error.localizedDescription)
                pending?.markFailed()
            }
        }
        // ADR-0073 R.13a — the lineage is not in `snapshotParts()`: a
        // catch-up is the snapshot parts PLUS every lineage envelope, each
        // with its original stamp.
        for envelope in (try? syncedState.lineageEnvelopes()) ?? [] {
            await sendServiceEnvelope(envelope)
        }
        // ADR-0075 — nor is the recovery-email register: it is re-sent with
        // its original stamp beside them.
        if let envelope = (try? syncedState.recoveryEmailEnvelope()).flatMap({ $0 }) {
            await sendServiceEnvelope(envelope)
        }
        // ADR-0076 — and the own-username register.
        if let envelope = (try? syncedState.ownUsernameEnvelope()).flatMap({ $0 }) {
            await sendServiceEnvelope(envelope)
        }
        // D3 — and the quick reaction.
        if let envelope = (try? syncedState.quickReactionEnvelope()).flatMap({ $0 }) {
            await sendServiceEnvelope(envelope)
        }
        // ADR-0079 — and every link record, with its original stamp.
        for envelope in (try? syncedState.linkRecordEnvelopes()) ?? [] {
            await sendServiceEnvelope(envelope)
        }
        // UX 17.6 — and every device acknowledgement, so the newcomer's
        // synced state (and its backup) carries them.
        for envelope in (try? syncedState.deviceAckEnvelopes()) ?? [] {
            await sendServiceEnvelope(envelope)
        }
    }

    // MARK: - ADR-0079: the link-record register

    static func linkRecordsUnsentKey(accountIdB64: String) -> String {
        "lakko.link-records.unsent.v1.\(accountIdB64)"
    }

    func flushUnsentLinkRecords() async {
        guard let account = activeAccountIdB64 else { return }
        let key = Self.linkRecordsUnsentKey(accountIdB64: account)
        guard UserDefaults.standard.object(forKey: key) != nil else { return }
        do {
            try await sendLineageEnvelopes((try? syncedState.linkRecordEnvelopes()) ?? [])
            UserDefaults.standard.removeObject(forKey: key)
        } catch {
            NSLog("ADR-0079: link-record register send failed; retried at the next start")
        }
    }

    /// Fold a write here at once, persist it, mark it unsent and send the
    /// one record to the account's other devices.
    private func afterLinkRecordWrite(linkId: Data) async {
        foldLinkRecordWrite()
        await sendLinkRecord(linkId: linkId)
    }

    /// The local half of ``afterLinkRecordWrite(linkId:)``: fold, persist and
    /// mark the register unsent (a start retries it).
    private func foldLinkRecordWrite() {
        stateChanged()
        persistSyncState()
        guard let account = activeAccountIdB64 else { return }
        UserDefaults.standard.set(true, forKey: Self.linkRecordsUnsentKey(accountIdB64: account))
    }

    /// The network half: send the one record, then clear the unsent mark.
    private func sendLinkRecord(linkId: Data) async {
        guard let account = activeAccountIdB64 else { return }
        let key = Self.linkRecordsUnsentKey(accountIdB64: account)
        do {
            if let envelope = try syncedState.linkRecordEnvelopeFor(linkId: linkId) {
                try await sendLineageEnvelopes([envelope])
            }
            UserDefaults.standard.removeObject(forKey: key)
        } catch {
            NSLog("ADR-0079: link-record register send failed; retried at the next start")
        }
    }

    /// Record a link just minted (ADR-0079 section 4). `false` when the
    /// synced state is not loaded or the record is refused.
    @discardableResult
    public func setLinkRecord(_ record: LinkRecord) async -> Bool {
        guard ready, !deviceRemoved else { return false }
        let written: Bool
        do {
            written = try syncedState.setLinkRecord(hlc: Hlc.mint(), record: record)
        } catch {
            NSLog("ADR-0079: link record refused")
            return false
        }
        if written { await afterLinkRecordWrite(linkId: record.linkId) }
        return written
    }

    /// Mark a link revoked (final). `false` when nothing changed.
    @discardableResult
    public func revokeLinkRecord(linkId: Data) async -> Bool {
        guard ready, !deviceRemoved else { return false }
        let written = (try? syncedState.revokeLinkRecord(hlc: Hlc.mint(), linkId: linkId)) ?? false
        if written { await afterLinkRecordWrite(linkId: linkId) }
        return written
    }

    /// Mark a link copied (final; `docs/ux/contact-invite-link.md` section 9):
    /// from then on no device of the account offers to edit it. Sent to the
    /// other devices like a revoke. `false` when nothing changed.
    @discardableResult
    public func markLinkShared(linkId: Data) async -> Bool {
        guard ready, !deviceRemoved else { return false }
        let written = (try? syncedState.markLinkShared(hlc: Hlc.mint(), linkId: linkId)) ?? false
        guard written else { return false }
        // Folded and persisted before this returns, so Copy writes the
        // clipboard only once this device has stopped offering the edit; the
        // send to the other devices does not hold the clipboard back (an
        // unsent register is retried at the next start).
        foldLinkRecordWrite()
        Task { [weak self] in await self?.sendLinkRecord(linkId: linkId) }
        return true
    }

    /// Count one request accepted through a link, remembering the requester.
    @discardableResult
    public func recordLinkUse(linkId: Data, requesterKey: Data) async -> Bool {
        guard ready, !deviceRemoved else { return false }
        let written = (try? syncedState.recordLinkUse(
            hlc: Hlc.mint(), linkId: linkId, requesterKey: requesterKey)) ?? false
        if written { await afterLinkRecordWrite(linkId: linkId) }
        return written
    }

    /// The one rule for a request's link answer (ADR-0079 section 3 as
    /// amended 2026-10-19), asked of the fold: the link's expiry is judged at
    /// `pressedAtMs` (the time the DS stamped on the request row), its
    /// revocation and uses now. `nil` while the fold is not loaded: the
    /// request stays undecided (no timer) and is decided once ``ready`` turns
    /// `true`.
    public func linkAnswerVerdictAt(
        linkId: Data, linkSecret: Data, requesterKey: Data, pressedAtMs: UInt64
    ) -> LinkAnswerVerdict? {
        guard ready else { return nil }
        return (try? syncedState.linkAnswerVerdict(
            linkId: linkId, linkSecret: linkSecret, requesterKey: requesterKey, pressedAtMs: pressedAtMs)) ?? .dead
    }

    // MARK: - UX 17.5 / 17.6: the device-acknowledgement register

    /// `UserDefaults` marker: an acknowledgement written here has not been
    /// confirmed sent (the own-username marker's pattern).
    static func deviceAcksUnsentKey(accountIdB64: String) -> String {
        "lakko.device-acks.unsent.v1.\(accountIdB64)"
    }

    private var deviceAcksUnsent: Bool {
        guard let account = activeAccountIdB64 else { return false }
        return UserDefaults.standard.object(forKey: Self.deviceAcksUnsentKey(accountIdB64: account)) != nil
    }

    /// Send every acknowledgement this device holds again when an earlier
    /// send was not confirmed (registers are write-once, so a re-send is a
    /// no-op on a device that has them).
    func flushUnsentDeviceAcks() async {
        guard let account = activeAccountIdB64 else { return }
        let key = Self.deviceAcksUnsentKey(accountIdB64: account)
        guard UserDefaults.standard.object(forKey: key) != nil else { return }
        do {
            try await sendLineageEnvelopes((try? syncedState.deviceAckEnvelopes()) ?? [])
            UserDefaults.standard.removeObject(forKey: key)
        } catch {
            NSLog("device-ack: register send failed; retried")
        }
    }

    /// Acknowledge `keys` (32-byte device signature keys) for the account:
    /// write the registers (write-once), persist at once and send the new
    /// envelopes to the account's other devices. A send that fails is owed
    /// and retried by the poll loop.
    func acknowledgeDevices(_ keys: [Data]) {
        guard ready, !deviceRemoved, let account = activeAccountIdB64, !keys.isEmpty else { return }
        let envelopes: [Data]
        do {
            envelopes = try syncedState.acknowledgeDevices(
                hlc: Hlc.mint(), deviceKeys: keys,
                nowMs: UInt64(max(0, Date().timeIntervalSince1970 * 1000)))
        } catch {
            NSLog("device-ack: register refused")
            return
        }
        guard !envelopes.isEmpty else { return }
        stateChanged()
        persistSyncState()
        UserDefaults.standard.set(true, forKey: Self.deviceAcksUnsentKey(accountIdB64: account))
        Task { @MainActor [weak self] in
            guard let self else { return }
            do {
                try await self.sendLineageEnvelopes(envelopes)
                if self.activeAccountIdB64 == account {
                    UserDefaults.standard.removeObject(
                        forKey: Self.deviceAcksUnsentKey(accountIdB64: account))
                }
            } catch {
                // Set again: an earlier send that succeeded meanwhile may
                // have cleared the marker this write set.
                if self.activeAccountIdB64 == account {
                    UserDefaults.standard.set(true, forKey: Self.deviceAcksUnsentKey(accountIdB64: account))
                }
                NSLog("device-ack: register send failed; retried")
            }
        }
    }

    // MARK: - ADR-0076: the own-username register

    static func ownUsernameUnsentKey(accountIdB64: String) -> String {
        "lakko.own-username.unsent.v1.\(accountIdB64)"
    }

    func flushUnsentOwnUsername() async {
        guard let account = activeAccountIdB64 else { return }
        let key = Self.ownUsernameUnsentKey(accountIdB64: account)
        guard UserDefaults.standard.object(forKey: key) != nil else { return }
        do {
            if let envelope = try syncedState.ownUsernameEnvelope() {
                try await sendLineageEnvelopes([envelope])
            }
            UserDefaults.standard.removeObject(forKey: key)
        } catch {
            NSLog("ADR-0076: own-username register send failed; retried at the next start")
        }
    }

    /// Write `username` to the register (last writer wins), persist it at
    /// once and send it to the account's other devices. Returns `false`
    /// when the synced state is not loaded yet or the name is refused.
    @discardableResult
    public func setOwnUsername(_ username: String) async -> Bool {
        guard ready, !deviceRemoved, let account = activeAccountIdB64 else { return false }
        let written: Bool
        do {
            written = try syncedState.setOwnUsername(hlc: Hlc.mint(), username: username)
        } catch {
            NSLog("ADR-0076: own-username register refused")
            return false
        }
        guard written else { return true }
        stateChanged()
        persistSyncState()
        UserDefaults.standard.set(true, forKey: Self.ownUsernameUnsentKey(accountIdB64: account))
        do {
            if let envelope = try syncedState.ownUsernameEnvelope() {
                try await sendLineageEnvelopes([envelope])
            }
            UserDefaults.standard.removeObject(forKey: Self.ownUsernameUnsentKey(accountIdB64: account))
        } catch {
            NSLog("ADR-0076: own-username register send failed; retried at the next start")
        }
        return true
    }

    // MARK: - Reactions-and-emoji spec section 6: the quick-reaction register

    static func quickReactionUnsentKey(accountIdB64: String) -> String {
        "lakko.quick-reaction.unsent.v1.\(accountIdB64)"
    }

    /// The account's quick reaction, `nil` for the default.
    public var quickReaction: String? { (try? syncedState.quickReaction()).flatMap { $0 } }

    /// Send the register again when an earlier send was not confirmed.
    func flushUnsentQuickReaction() async {
        guard let account = activeAccountIdB64 else { return }
        let key = Self.quickReactionUnsentKey(accountIdB64: account)
        guard UserDefaults.standard.object(forKey: key) != nil else { return }
        do {
            if let envelope = try syncedState.quickReactionEnvelope() {
                try await sendLineageEnvelopes([envelope])
            }
            UserDefaults.standard.removeObject(forKey: key)
        } catch {
            NSLog("quick-reaction: register send failed; retried at the next start")
        }
    }

    /// D3 — write the account's quick reaction (last writer wins), persist it
    /// and send it to the account's other devices.
    @discardableResult
    public func setQuickReaction(_ emoji: String) async -> Bool {
        guard ready, !deviceRemoved, let account = activeAccountIdB64 else { return false }
        let written: Bool
        do {
            written = try syncedState.setQuickReaction(hlc: Hlc.mint(), emoji: emoji)
        } catch {
            NSLog("quick-reaction: register refused")
            return false
        }
        guard written else { return true }
        stateChanged()
        persistSyncState()
        UserDefaults.standard.set(true, forKey: Self.quickReactionUnsentKey(accountIdB64: account))
        do {
            if let envelope = try syncedState.quickReactionEnvelope() {
                try await sendLineageEnvelopes([envelope])
            }
            UserDefaults.standard.removeObject(forKey: Self.quickReactionUnsentKey(accountIdB64: account))
        } catch {
            NSLog("quick-reaction: register send failed; retried at the next start")
        }
        return true
    }

    // MARK: - ADR-0075: the recovery-email register

    /// `UserDefaults` marker: the register written here has not been
    /// confirmed sent (the lineage marker's pattern).
    static func recoveryEmailUnsentKey(accountIdB64: String) -> String {
        "lakko.recovery-email.unsent.v1.\(accountIdB64)"
    }

    /// Send the register again when an earlier send was not confirmed.
    func flushUnsentRecoveryEmail() async {
        guard let account = activeAccountIdB64 else { return }
        let key = Self.recoveryEmailUnsentKey(accountIdB64: account)
        guard UserDefaults.standard.object(forKey: key) != nil else { return }
        do {
            if let envelope = try syncedState.recoveryEmailEnvelope() {
                try await sendLineageEnvelopes([envelope])
            }
            UserDefaults.standard.removeObject(forKey: key)
        } catch {
            NSLog("ADR-0075: recovery-email register send failed; retried at the next start")
        }
    }

    /// Keep `address` as the account's recovery email (`nil` = removed):
    /// write the register (last writer wins; the core normalises), persist
    /// it at once, and send it to the account's other devices. Returns
    /// `false` when the account's synced state is not loaded yet (the caller
    /// tries again once it is) or the address is refused.
    @discardableResult
    public func setRecoveryEmail(_ address: String?) async -> Bool {
        guard ready, !deviceRemoved, let account = activeAccountIdB64 else { return false }
        let written: Bool
        do {
            written = try syncedState.setRecoveryEmail(hlc: Hlc.mint(), address: address)
        } catch {
            NSLog("ADR-0075: recovery-email register refused")
            return false
        }
        guard written else { return true }
        stateChanged()
        // The register must survive a kill right after the write.
        persistSyncState()
        UserDefaults.standard.set(true, forKey: Self.recoveryEmailUnsentKey(accountIdB64: account))
        do {
            if let envelope = try syncedState.recoveryEmailEnvelope() {
                try await sendLineageEnvelopes([envelope])
            }
            UserDefaults.standard.removeObject(forKey: Self.recoveryEmailUnsentKey(accountIdB64: account))
        } catch {
            NSLog("ADR-0075: recovery-email register send failed; retried at the next start")
        }
        return true
    }

    // MARK: - ADR-0073 R.5: the lineage register

    /// `UserDefaults` marker: a lineage register written here has not been
    /// confirmed sent. Set before the publish and cleared after it, so a
    /// device that was offline (or closed mid-send) sends every register
    /// again at its next start. Re-sending is idempotent: the fold keeps the
    /// first stamp.
    static func lineageUnsentKey(accountIdB64: String) -> String {
        "lakko.lineage.unsent.v1.\(accountIdB64)"
    }

    /// Publish each envelope as its own DeviceGroup service envelope. Throws
    /// on the first failure (unlike ``sendServiceEnvelope(_:)``, the caller
    /// needs to know whether the register left the device).
    private func sendLineageEnvelopes(_ envelopes: [Data]) async throws {
        for envelope in envelopes {
            guard ready, group != nil, !deviceRemoved else {
                throw DeviceGroupSyncError.notReady
            }
            try await publish { group, identity in
                try group.createServiceEnvelopeMessage(
                    identity: identity, envelope: envelope, hlc: Hlc.mint())
            }
        }
    }

    /// Send every register again when an earlier send was not confirmed.
    func flushUnsentLineage() async {
        guard let account = activeAccountIdB64 else { return }
        let key = Self.lineageUnsentKey(accountIdB64: account)
        guard UserDefaults.standard.object(forKey: key) != nil else { return }
        do {
            try await sendLineageEnvelopes(try syncedState.lineageEnvelopes())
            UserDefaults.standard.removeObject(forKey: key)
        } catch {
            NSLog("ADR-0073: lineage register send failed; retried at the next start")
        }
    }

    /// Write the register `successor -> root conversation` (write-once: the
    /// first stamp is kept) and send it to the account's other devices.
    /// Returns whether this call wrote it.
    @discardableResult
    public func setLineage(successorGroupId: Data, rootConversationId: Data) async -> Bool {
        let written: Bool
        do {
            written = try syncedState.setLineage(
                hlc: Hlc.mint(),
                successorGroupId: successorGroupId,
                rootConversationId: rootConversationId)
        } catch {
            // An id out of bounds, or a successor naming itself.
            NSLog("ADR-0073: lineage register refused: %@", error.localizedDescription)
            return false
        }
        guard written else { return false }
        stateChanged()
        // The register must survive a kill right after the write: the
        // debounced persist would lose it.
        persistSyncState()
        let account = activeAccountIdB64
        if let account {
            UserDefaults.standard.set(true, forKey: Self.lineageUnsentKey(accountIdB64: account))
        }
        do {
            if let envelope = try syncedState.lineageEnvelopeFor(successorGroupId: successorGroupId) {
                try await sendLineageEnvelopes([envelope])
            }
            if let account {
                UserDefaults.standard.removeObject(forKey: Self.lineageUnsentKey(accountIdB64: account))
            }
        } catch {
            NSLog("ADR-0073: lineage register send failed; retried at the next start")
        }
        return true
    }

    /// This device changed the DeviceGroup through a handle of its own (the
    /// pairing scan, a removal): reload the handle from the snapshot that flow
    /// persisted, and treat the change like a received commit. Without the
    /// reload this repository would keep sealing sync messages at the epoch
    /// before its own commit.
    public func reloadAfterLocalMembershipChange(secrets: IdentitySecrets) {
        guard let accountIdB64 = activeAccountIdB64,
              accountIdB64 == base64Encode(secrets.accountId) else { return }
        // ADR-0081 — the commit outbox already reloaded the handle when the
        // commit landed (``lakkoDeviceGroupChangedHere``); a second reload is
        // a no-op on the roster.
        reloadFromDisk(accountIdB64: accountIdB64)
    }
}

/// Why a DeviceGroup publish that must report its outcome could not run.
enum DeviceGroupSyncError: Error {
    /// No ready handle (not bootstrapped, reloading, or this device removed).
    case notReady
}

extension DeviceGroupSyncRepository: ConversationEndSync {}
