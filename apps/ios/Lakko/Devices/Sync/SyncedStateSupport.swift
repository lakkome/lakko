// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The pieces of the ADR-0058 synced state that are not the repository's
/// runtime: where the bytes live, how two states merge, when the DeviceGroup
/// grew, and what is still waiting to be sent. Pure or plain-I/O, so a test
/// can run them without a DeviceGroup.

// MARK: - Persistence

/// The folded synced state on this device's disk: `SyncedState.toBytes()`,
/// sealed by ``DeviceGroupSnapshotStore`` under the Keychain wrap key in its
/// own folder (a draft is a message the person has not sent yet).
///
/// It replaced the JSON `DeviceGroupSyncStateCodec` store in the same folder.
/// There is no migration, here or between versions of the `SyncedState`
/// encoding: a blob this build's `SyncedState.fromBytes` refuses (the removed
/// codec's JSON, or the version 1 encoding that ADR-0065 replaced with
/// version 2) is "no synced state held". It is reported once per account per
/// launch, never handed to a backup, and the next write replaces it.
public enum SyncedStateStore {

    static let directoryName = "lakko-device-sync-state"

    /// What a backup's synced state did to the store.
    public enum MergeOutcome: Equatable {
        /// The bytes are not a synced state this build reads: nothing was
        /// merged and the store was not touched.
        case unreadable
        /// The bytes were folded in; `changed` says whether any register moved.
        case merged(changed: Bool)
    }

    static func store() -> DeviceGroupSnapshotStore {
        DeviceGroupSnapshotStore(directoryName: directoryName)
    }

    /// The accounts whose unreadable stored blob was already reported in this
    /// process, so a blob that waits for its replacing write is logged once.
    private static let reportedLock = NSLock()
    private static var reportedUnreadable: Set<String> = []

    /// The stored blob and the state it decodes to, or `nil` when nothing is
    /// stored or what is stored is refused.
    private static func readStored(accountIdB64: String) -> (bytes: Data, state: SyncedState)? {
        guard let blob = store().getSnapshot(accountIdB64: accountIdB64) else { return nil }
        do {
            return (blob, try SyncedState.fromBytes(bytes: blob))
        } catch {
            let first = reportedLock.withLock { reportedUnreadable.insert(accountIdB64).inserted }
            if first {
                NSLog("SyncedStateStore: stored state unreadable, treated as none held: %@",
                      error.localizedDescription)
            }
            return nil
        }
    }

    /// The persisted state, or an empty one when nothing (readable) is stored.
    public static func load(accountIdB64: String) -> SyncedState {
        readStored(accountIdB64: accountIdB64)?.state ?? SyncedState()
    }

    /// The persisted bytes, for a backup. `nil` when none is stored or the
    /// stored blob is refused: the core's backup composer refuses a payload
    /// whose synced state the fold refuses, so offering a refused blob would
    /// fail the whole upload, contacts and conversation list included.
    public static func bytes(accountIdB64: String) -> Data? {
        readStored(accountIdB64: accountIdB64)?.bytes
    }

    public static func save(_ state: SyncedState, accountIdB64: String) {
        do {
            store().putSnapshot(accountIdB64: accountIdB64, blob: try state.toBytes())
            // What is stored is readable again; a later refusal is a new event.
            reportedLock.withLock { _ = reportedUnreadable.remove(accountIdB64) }
        } catch {
            NSLog("SyncedStateStore: encode failed: %@", error.localizedDescription)
        }
    }

    public static func delete(accountIdB64: String) {
        store().deleteSnapshot(accountIdB64: accountIdB64)
        reportedLock.withLock { _ = reportedUnreadable.remove(accountIdB64) }
    }

    /// Fold `bytes` (a backup's synced state) into whatever is stored for the
    /// account, and store the result. Merging rather than replacing: every
    /// register keeps whichever stamp is newer, so a restore can never undo a
    /// write this device already holds.
    ///
    /// Bytes this build refuses (a backup made with the version 1 encoding)
    /// are a backup that holds no synced state: nothing is merged, nothing is
    /// written, and the restore carries on.
    @discardableResult
    public static func mergeIntoStored(bytes: Data, accountIdB64: String) -> MergeOutcome {
        let incoming: SyncedState
        do {
            incoming = try SyncedState.fromBytes(bytes: bytes)
        } catch {
            NSLog("SyncedStateStore: backup state unreadable, treated as none held: %@",
                  error.localizedDescription)
            return .unreadable
        }
        let stored = load(accountIdB64: accountIdB64)
        let changed = SyncedStateMerge.merge(incoming, into: stored)
        save(stored, accountIdB64: accountIdB64)
        return .merged(changed: changed)
    }
}

extension Notification.Name {
    /// Posted after a backup restore merged a synced state into the store, so a
    /// running ``DeviceGroupSyncRepository`` for that account folds it too.
    /// `userInfo["accountIdB64"]` is the account, `userInfo["bytes"]` the state.
    static let lakkoSyncedStateRestored = Notification.Name("me.lakko.synced-state-restored")
}

// MARK: - Merge

public enum SyncedStateMerge {

    /// Fold every register of `source` into `target`, each with its original
    /// stamp, through the core's own merge (`SyncedState::merge_from`). The
    /// fold is idempotent and commutative, so this is a true merge.
    ///
    /// EVERY register, the conversation lineage included (ADR-0073 R.5).
    /// This used to go through the growth re-send's snapshot parts, which
    /// deliberately leave the lineage out (it travels in its own envelope):
    /// a backup carried the lineage and a restore dropped it, so a restored
    /// device drew a moved conversation as two.
    @discardableResult
    public static func merge(_ source: SyncedState, into target: SyncedState) -> Bool {
        do {
            return try target.mergeFrom(other: source)
        } catch {
            NSLog("SyncedStateMerge failed: %@", error.localizedDescription)
            return false
        }
    }
}

// MARK: - Growth (ADR-0058 section 5.1)

public enum DeviceGroupGrowth {

    /// The leaf indexes of the members BEFORE the group grew, or `nil` when
    /// `after` holds no device `before` did not.
    ///
    /// Compared by signature key, not by leaf index: OpenMLS hands a vacated
    /// leaf to the next device added, so a removal and an add in one commit
    /// keeps the index set equal while a new device has arrived.
    public static func priorLeavesIfGrew(
        before: [DeviceLeaf], after: [DeviceLeaf]
    ) -> [UInt32]? {
        let known = Set(before.map(\.signaturePublicKey))
        guard after.contains(where: { !known.contains($0.signaturePublicKey) }) else {
            return nil
        }
        return before.map(\.leafIndex)
    }

    /// Whether THIS device publishes the catch-up snapshot for that growth:
    /// only the lowest leaf among the prior members (the shared rule).
    public static func shouldResend(
        ownLeaf: UInt32, before: [DeviceLeaf], after: [DeviceLeaf]
    ) -> Bool {
        guard let prior = priorLeavesIfGrew(before: before, after: after),
              prior.contains(ownLeaf) else { return false }
        return LakkoCore.shouldSendSnapshot(ownLeaf: ownLeaf, priorMemberLeaves: prior)
    }

    /// The random wait before a re-send, up to 30 seconds, so the new
    /// device's own first writes are not raced.
    public static func randomDelayNanoseconds() -> UInt64 {
        UInt64.random(in: 0...30_000_000_000)
    }
}

// MARK: - Pending sync (offline writes)

/// Whether something folded on this device has not reached the other devices.
///
/// A write applies here at once and travels later (UX spec chat-folders 6).
/// Rather than queue the failed messages, the device remembers only THAT a
/// publish failed, per account, in `UserDefaults` - a boolean says nothing
/// about the person's chats, so it needs no sealing - and after the next poll
/// fetch that succeeds it publishes its whole folded state as catch-up parts.
/// Every register keeps its original stamp, so the re-send carries each
/// missed write exactly as a queue would have, and survives a relaunch.
///
/// The flag is cleared only when no publish failed while that flush ran: a
/// write that failed during the flush is newer than what the flush may have
/// sent, and clearing would lose it.
@MainActor
public final class PendingSyncTracker {

    private let defaults: UserDefaults
    private let key: String
    private let accountIdB64: String
    /// Failures seen in this process; a flush compares it before and after.
    private var failures: UInt64 = 0

    public init(accountIdB64: String, defaults: UserDefaults = .standard) {
        self.defaults = defaults
        self.key = "lakko.deviceSync.pending." + accountIdB64
        self.accountIdB64 = accountIdB64
    }

    public var isPending: Bool { defaults.bool(forKey: key) }

    /// A publish failed: remember it across a relaunch.
    ///
    /// The flag is not written once the account's session has ended
    /// (``AccountStoreGate``): the failure is learned after a publish's wait.
    public func markFailed() {
        failures &+= 1
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(true, forKey: key)
    }

    /// Start a flush; hand the token back to ``finishFlush(_:)``.
    public func beginFlush() -> UInt64 { failures }

    /// End a flush. Clears the flag only when nothing failed since `token`.
    /// Returns whether the flag is now clear.
    @discardableResult
    public func finishFlush(_ token: UInt64) -> Bool {
        guard failures == token else { return false }
        defaults.removeObject(forKey: key)
        return true
    }

    /// Forget the flag (logout / forget this device).
    public func clear() {
        defaults.removeObject(forKey: key)
    }
}
