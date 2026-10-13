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
///     loads the DeviceGroup handle from the existing snapshot
///     or bootstraps a founder leaf, kicks off the poll loop.
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

    /// UX spec chat-folders section 6 — something folded here is still
    /// waiting to reach the other devices (this device is offline).
    @Published public private(set) var pendingSync: Bool = false

    /// Another device removed this one from the account, and this device has
    /// applied that removal (its DeviceGroup is inactive). The poll loop has
    /// stopped for good; the app root shows "This device was removed"
    /// (docs/ux/second-device.md section 3.1). Reset on sign-out and on an
    /// account switch.
    @Published public private(set) var deviceRemoved: Bool = false

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
    /// The raw account id of the active session, for re-founding the
    /// DeviceGroup (``repairForkedDeviceGroup``).
    private var activeAccountId: Data?
    /// When the current fork suspicion started (``DeviceGroupFork``), `nil`
    /// when there is none.
    private var forkSuspectSince: Int64?
    /// Accounts whose forked DeviceGroup this process already tried to
    /// repair: at most one repair per session per account. Deliberately not
    /// reset by ``clear()``.
    private var forkRepairAttempted: Set<String> = []
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

    public init() {
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
        } catch {
            NSLog("DeviceGroupSync bootstrap: ensureDeviceGroup %@", error.localizedDescription)
            activeAccountIdB64 = nil
            return
        }
        identity = ensured.identity
        group = ensured.group

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
        pendingServiceEnvelopes = [:]
        activeAccountIdB64 = nil
        activeAccountId = nil
        ready = false
        deviceRemoved = false
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
        if d != drafts { drafts = d }
        if f != flags { flags = f }
        if c != readCursors { readCursors = c }
        if o != order { order = o }
        if folderList != folders { folders = folderList }
        let isPending = pending?.isPending ?? false
        if pendingSync != isPending { pendingSync = isPending }
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
        identity = nil
        group = nil
        forkSuspectSince = nil
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
        forkSuspectSince = nil
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

    private func sendBody(_ body: DeviceGroupBody, hlc: UInt64) async throws {
        try await publish { group, identity in
            try group.createDevicegroupMessage(identity: identity, body: body, hlc: hlc)
        }
    }

    /// Encrypt with `compose` under the DeviceGroup's current epoch and
    /// publish it on the DeviceGroup journal.
    private func publish(
        _ compose: (DeviceGroup, Identity) throws -> Data
    ) async throws {
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
        // published: a failed write, a session that ended, or a handle of an
        // earlier session all stop here.
        try DeviceGroupLifecycle.persistSnapshotOrThrow(
            identity: identity, accountIdB64: accountIdB64)
        let groupId = try group.groupId()
        let ds = DsClient()
        // G-13 (ADR-0015): wrap in the journal envelope (application messages
        // carry no attestation).
        let envelope = DeviceGroupJournalEnvelope.encode(
            message: wireBytes,
            attestation: nil
        )
        _ = try await ds.publishGroup(
            groupId: groupId,
            payload: envelope,
            auth: DeviceGroupDsAccess.auth(
                group: group, identity: identity, accountIdB64: accountIdB64, ds: ds)
        )
        DeviceGroupLifecycle.persistSnapshot(
            identity: identity,
            accountIdB64: accountIdB64
        )
    }

    // MARK: - Inbound poll loop

    private func runPollLoop(accountIdB64: String, groupId: Data, startCursor: Int64 = 0) async {
        let ds = DsClient()
        var cursor: Int64 = startCursor
        // This loop reads ``identity`` and ``group`` afresh after every wait.
        // Once its binding has moved they are another account's (or the same
        // account's next session's): the batch in hand is then dropped and
        // the loop ends, whether or not the cancellation reached the request.
        let bound = binding.capture()
        while !Task.isCancelled, bound.isCurrent {
            if !pendingServiceEnvelopes.isEmpty {
                await flushPendingServiceEnvelopes()
                guard bound.isCurrent else { return }
            }
            let result: FetchResult
            // ADR-0053 point 1 — the read is signed under the DeviceGroup's
            // current epoch.
            let auth = group.flatMap { g in
                identity.flatMap {
                    DeviceGroupDsAccess.auth(group: g, identity: $0, accountIdB64: accountIdB64, ds: ds)
                }
            }
            do {
                result = try await ds.fetchGroup(
                    groupId: groupId,
                    after: cursor,
                    limit: nil,
                    waitSecs: 25,
                    auth: auth
                )
            } catch DsClientError.http(let status, _, _) where status == 404 {
                continue
            } catch let err as DsClientError {
                if case .network = err {
                    try? await Task.sleep(nanoseconds: 2_000_000_000)
                    continue
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
                case .application(let data, _, _):
                    // ADR-0023 added the sender leaf to the
                    // Application case; the DeviceGroup sync path
                    // doesn't authorise on it (removals are gated
                    // upstream by the attestation verify), so it's
                    // ignored here.
                    do {
                        let parsed = try parseDevicegroupPayload(plaintext: data)
                        applyParsed(parsed)
                    } catch {
                        NSLog("DeviceGroupSync parseDevicegroupPayload failed: %@", error.localizedDescription)
                    }
                case .commit:
                    noteMembershipChange(before: devicesBefore, group: g)
                case .proposal, .externalJoinProposal:
                    // No application-state mutation.
                    break
                }
            }

            if mutated {
                if let id = identity {
                    DeviceGroupLifecycle.persistSnapshot(
                        identity: id,
                        accountIdB64: accountIdB64
                    )
                    // ADR-0057 — no heal-on-apply: the device that committed
                    // registered the epoch under the account's root.
                }
            }

            // A row in this batch removed THIS device (docs/ux/second-device.md
            // section 3.1). The batch that carried the removal was persisted
            // above, so the next launch finds the inactive snapshot and shows
            // the same screen; nothing after it is polled or persisted.
            if !Task.isCancelled, let g = group,
               noteDeviceGroupActivity(RemovedDevice.readActive { try g.isActive() }) {
                return
            }

            // Live walk 2026-09-30 — a DeviceGroup that forked from the one
            // on the DS (``DeviceGroupFork``) repairs itself, once.
            let drained = result.messages.isEmpty && !result.hasMore
            var localEpoch: UInt64?
            if let g = group, let id = identity {
                localEpoch = try? g.dsAccessSigner(identity: id).epoch()
            }
            if let localEpoch {
                let verdict = DeviceGroupFork.verdict(
                    localEpoch: localEpoch,
                    drained: drained,
                    latestEpoch: result.latestEpoch,
                    suspectSince: forkSuspectSince,
                    now: Int64(Date().timeIntervalSince1970 * 1000))
                switch verdict {
                case .inSync:
                    forkSuspectSince = nil
                case .suspect(let since):
                    forkSuspectSince = since
                case .forked:
                    forkSuspectSince = nil
                    // The repair re-founds the account's DeviceGroup; it never
                    // runs on a removed device. An inactive group cannot reach
                    // this point anyway (the loop stopped above, and its epoch
                    // comes from a DS signer an inactive group cannot derive -
                    // the exporter refuses - so `localEpoch` is nil and no
                    // verdict is taken); the guard keeps it so if either of
                    // those changes.
                    guard let g = group,
                          RemovedDevice.forkRepairAllowed(RemovedDevice.readActive({ try g.isActive() }))
                    else { break }
                    if await repairForkedDeviceGroup(
                        accountIdB64: accountIdB64,
                        groupId: groupId,
                        localEpoch: localEpoch,
                        latestEpoch: result.latestEpoch ?? 0,
                        cursor: cursor,
                        bound: bound
                    ) {
                        // The poll loop stopped: either a new one runs over
                        // the re-founded DeviceGroup, or the repair failed
                        // and the next session retries.
                        return
                    }
                }
            }
        }
    }

    // MARK: - Fork repair (live walk 2026-09-30)

    /// Re-found this account's DeviceGroup after it forked from the one on
    /// the DS: this device holds epoch `localEpoch` while the chain the
    /// account root registered is at `latestEpoch`, and the journal carries
    /// nothing more this device can apply.
    ///
    /// Only the DeviceGroup snapshot is deleted. It is a separate store
    /// (``DeviceGroupSnapshotStore``, its own folder under Application
    /// Support) holding the provider of the DeviceGroup's own `Identity`; the
    /// conversation groups live in ``IdentitySnapshotStore`` under another
    /// `Identity` instance, and the device identity seed, the device binding,
    /// contacts and the synced state (``SyncedStateStore``, its own folder) are
    /// all untouched. The device identity is the same seed and signature key,
    /// so this device's leaves in conversation groups are unaffected. iOS keeps
    /// no DeviceGroup journal cursor on disk (the poll starts from 0 each
    /// session), so there is none to delete.
    ///
    /// The founding path then runs as for a new account: `createFounder`, and
    /// the founding mark makes the claim a claim-or-reset under the account
    /// root (ADR-0057 obligation 7), which replaces the old chain.
    ///
    /// Consequence: every OTHER device of the account loses the DeviceGroup.
    /// Its epoch keys are gone after the reset, so its reads answer `401` and
    /// it must be added again. A `401` is not a drained fetch, so those devices
    /// never trigger a repair of their own.
    ///
    /// Returns `true` when this poll loop must stop (the repair ran, whether
    /// or not it succeeded), `false` when nothing was changed and polling
    /// continues.
    private func repairForkedDeviceGroup(
        accountIdB64: String,
        groupId: Data,
        localEpoch: UInt64,
        latestEpoch: UInt64,
        cursor: Int64,
        bound: BindingEpoch.Captured
    ) async -> Bool {
        guard !deviceRemoved,
              let held = group,
              RemovedDevice.forkRepairAllowed(RemovedDevice.readActive({ try held.isActive() }))
        else { return false }
        guard !forkRepairAttempted.contains(accountIdB64) else {
            NSLog("DeviceGroupSync fork persists (local epoch %llu, DS epoch %llu); already repaired this session",
                  localEpoch, latestEpoch)
            return false
        }
        forkRepairAttempted.insert(accountIdB64)
        NSLog("DeviceGroupSync DeviceGroup forked: local epoch %llu, DS epoch %llu; re-founding",
              localEpoch, latestEpoch)
        guard let accountId = activeAccountId,
              activeAccountIdB64 == accountIdB64 else { return true }
        // The reset is signed by the journal root, derived from the account
        // identity seed. Without it nothing could be claimed, so nothing is
        // deleted either.
        guard DeviceGroupDsAccess.rootSigner(accountIdB64: accountIdB64, groupId: groupId) != nil else {
            NSLog("DeviceGroupSync fork repair skipped: the account identity seed is not on this device")
            return false
        }

        // Stop this poll loop: from here on it runs no further.
        resendTask?.cancel()
        resendTask = nil
        identity = nil
        group = nil

        DeviceGroupSnapshotStore().deleteSnapshot(accountIdB64: accountIdB64)
        let ensured: DeviceGroupLifecycle.Ensured
        do {
            let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(accountIdB64: accountIdB64)
            ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
                accountIdBytes: accountId,
                accountIdB64: accountIdB64,
                deviceIdentitySeed: seed)
        } catch {
            NSLog("DeviceGroupSync fork repair: founding failed, retried next session: %@",
                  error.localizedDescription)
            return true
        }
        guard ensured.bootstrapped, (try? ensured.group.groupId()) == groupId else {
            NSLog("DeviceGroupSync fork repair: the DeviceGroup was not re-founded; retried next session")
            return true
        }

        // The claim: the founding mark set by ``DeviceGroupLifecycle`` turns
        // it into claim-or-reset, and it is cleared only when one landed.
        let ds = DsClient()
        await DeviceGroupDsAccess.claim(
            group: ensured.group, identity: ensured.identity,
            accountIdB64: accountIdB64, ds: ds)
        if JournalFoundingMarks.shared.isMarked(journalIdHex: DsClient.pseudonymHex(groupId)) {
            // Fail closed: the re-founded group is not on the DS. Nothing is
            // sent under it (``DeviceGroupDsAccess/requireFoundingClaim``), and
            // the next session's open claims-or-resets again because the mark
            // is still set.
            NSLog("DeviceGroupSync fork repair: the journal reset did not land; retried next session")
            return true
        }
        // The epoch and not the account id alone: the same account signed out
        // and in again while the claim was out has a runtime of its own.
        guard !Task.isCancelled, bound.isCurrent, activeAccountIdB64 == accountIdB64 else { return true }

        identity = ensured.identity
        group = ensured.group
        NSLog("DeviceGroupSync fork repair: DeviceGroup re-founded at epoch %llu; other devices must be added again",
              (try? ensured.group.dsAccessSigner(identity: ensured.identity).epoch()) ?? 0)
        // The old group's rows cannot be read by the new group: resume at the
        // journal head this loop last saw.
        pollTask = Task { [weak self] in
            await self?.runPollLoop(
                accountIdB64: accountIdB64, groupId: groupId, startCursor: cursor)
        }
        return true
    }

    /// Fold one payload another device sent. Every kind goes through the one
    /// fold, the ADR-0058 catch-up parts included; a payload that changes
    /// nothing (older, or already held) is a no-op.
    func applyParsed(_ parsed: ParsedDeviceGroupPayload) {
        if case .serviceEnvelope(let envelope) = parsed.body {
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
        guard let after = try? g.devices(), let own = try? g.ownLeafIndex() else { return }
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
    }

    /// This device changed the DeviceGroup through a handle of its own (the
    /// pairing scan, a removal): reload the handle from the snapshot that flow
    /// persisted, and treat the change like a received commit. Without the
    /// reload this repository would keep sealing sync messages at the epoch
    /// before its own commit.
    public func reloadAfterLocalMembershipChange(secrets: IdentitySecrets) {
        guard let accountIdB64 = activeAccountIdB64,
              accountIdB64 == base64Encode(secrets.accountId),
              !deviceRemoved else { return }
        let before = (try? group?.devices()) ?? []
        do {
            let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(accountIdB64: accountIdB64)
            let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
                accountIdBytes: secrets.accountId,
                accountIdB64: accountIdB64,
                deviceIdentitySeed: seed)
            identity = ensured.identity
            group = ensured.group
            if noteDeviceGroupActivity(RemovedDevice.readActive { try ensured.group.isActive() }) {
                return
            }
            noteMembershipChange(before: before, group: ensured.group)
            // ADR-0060 section 7 — a device was added: send it the setting,
            // the reach-key versions and (ADR-0048 section 4a) the own
            // profile, original stamps, through THIS handle, reloaded at the
            // post-add epoch. Never from the pairing's own handle: two handles
            // of one DeviceGroup sealing at the same sender generation reuse a
            // key and nonce (seen live on web 2026-09-30, SecretReuseError on
            // the new device).
            let after = (try? ensured.group.devices()) ?? []
            if after.count > before.count {
                let envelopes = Self.handOverEnvelopes(
                    accountIdB64: accountIdB64, reach: .shared, profiles: .shared)
                Task { @MainActor [weak self] in
                    for envelope in envelopes {
                        await self?.sendServiceEnvelope(envelope)
                    }
                }
            }
        } catch {
            NSLog("DeviceGroupSync reload failed: %@", error.localizedDescription)
        }
    }
}
