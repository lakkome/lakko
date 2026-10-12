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
/// There is no migration: an old blob does not parse as `SyncedState` bytes,
/// is treated as nothing stored, and the next write replaces it.
public enum SyncedStateStore {

    static let directoryName = "lakko-device-sync-state"

    static func store() -> DeviceGroupSnapshotStore {
        DeviceGroupSnapshotStore(directoryName: directoryName)
    }

    /// The persisted state, or an empty one when nothing (readable) is stored.
    public static func load(accountIdB64: String) -> SyncedState {
        guard let blob = store().getSnapshot(accountIdB64: accountIdB64) else {
            return SyncedState()
        }
        do {
            return try SyncedState.fromBytes(bytes: blob)
        } catch {
            NSLog("SyncedStateStore: stored state unreadable, starting empty: %@",
                  error.localizedDescription)
            return SyncedState()
        }
    }

    /// The persisted bytes as they are, for a backup. `nil` when none.
    public static func bytes(accountIdB64: String) -> Data? {
        store().getSnapshot(accountIdB64: accountIdB64)
    }

    public static func save(_ state: SyncedState, accountIdB64: String) {
        do {
            store().putSnapshot(accountIdB64: accountIdB64, blob: try state.toBytes())
        } catch {
            NSLog("SyncedStateStore: encode failed: %@", error.localizedDescription)
        }
    }

    public static func delete(accountIdB64: String) {
        store().deleteSnapshot(accountIdB64: accountIdB64)
    }

    /// Fold `bytes` (a backup's synced state) into whatever is stored for the
    /// account, and store the result. Merging rather than replacing: every
    /// register keeps whichever stamp is newer, so a restore can never undo a
    /// write this device already holds.
    @discardableResult
    public static func mergeIntoStored(bytes: Data, accountIdB64: String) -> Bool {
        let incoming: SyncedState
        do {
            incoming = try SyncedState.fromBytes(bytes: bytes)
        } catch {
            NSLog("SyncedStateStore: backup state unreadable: %@", error.localizedDescription)
            return false
        }
        let stored = load(accountIdB64: accountIdB64)
        let changed = SyncedStateMerge.merge(incoming, into: stored)
        save(stored, accountIdB64: accountIdB64)
        return changed
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
    /// EVERY register, the conversation lineage included (ADR-0065 R.5).
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
    /// Failures seen in this process; a flush compares it before and after.
    private var failures: UInt64 = 0

    public init(accountIdB64: String, defaults: UserDefaults = .standard) {
        self.defaults = defaults
        self.key = "lakko.deviceSync.pending." + accountIdB64
    }

    public var isPending: Bool { defaults.bool(forKey: key) }

    /// A publish failed: remember it across a relaunch.
    public func markFailed() {
        failures &+= 1
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
