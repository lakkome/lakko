// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation

/// Keep the encrypted backup current.
///
/// Until this existed the backup was written exactly twice in an account's
/// life - an empty baseline at signup and one more at recover - so everything
/// a person did afterwards (contacts accepted, groups joined, renames) never
/// reached the server blob, and a restore on a new device landed them back at
/// registration day. ``BackupClient/uploadFromCache(accountId:accountIdB64:ed25519Seed:contacts:knownGroupIdsHex:syncedState:)``
/// was built for precisely this and had no caller anywhere.
///
/// Mirror of the web `backup/auto-backup.ts`. The rules live here, pure and
/// clock-free, so they can be run by a test rather than observed on a device.
public enum AutoBackup {

    /// No more than one upload per this interval, however much changes.
    public static let minIntervalMs: Int64 = 5 * 60 * 1000

    /// How often the driver asks. A tick over a clean account reads two
    /// integers and returns, so this can be short enough to catch a change
    /// soon after it lands without costing anything when nothing has.
    public static let tickSeconds: UInt64 = 60

    /// What the driver remembers between runs, and across launches.
    public struct State: Equatable, Codable {
        /// When the last upload succeeded. `0` means never.
        public var lastUploadAtMs: Int64
        /// When the account first became worth backing up again, or `nil` when
        /// nothing has changed since the last upload.
        public var dirtySinceMs: Int64?
        /// The group set this device last reported, as a sorted join. `nil`
        /// before the first report - which counts as a change, because nothing
        /// here can vouch for what the server already holds.
        public var lastGroupsKey: String?
        /// The digest of the persisted synced state (ADR-0058) this device
        /// last reported. `nil` before the first report, which counts as a
        /// change for the same reason as ``lastGroupsKey``.
        public var lastSyncedStateKey: String?

        public init(
            lastUploadAtMs: Int64 = 0,
            dirtySinceMs: Int64? = nil,
            lastGroupsKey: String? = nil,
            lastSyncedStateKey: String? = nil
        ) {
            self.lastUploadAtMs = lastUploadAtMs
            self.dirtySinceMs = dirtySinceMs
            self.lastGroupsKey = lastGroupsKey
            self.lastSyncedStateKey = lastSyncedStateKey
        }

        public static let clean = State()
    }

    public enum Verdict: String, Equatable {
        case clean
        case throttled
        case due
    }

    /// Decide whether a backup should go out now.
    ///
    /// A clock that has moved BACKWARDS reads as due, which is the opposite of
    /// the choice `should_request_profiles` makes for its own throttle, and
    /// deliberately so: there, suppressing costs a display name; here,
    /// suppressing costs the user's data, and a stored `lastUploadAtMs` in the
    /// future would otherwise wedge backups off forever.
    public static func verdict(
        _ state: State,
        nowMs: Int64,
        minIntervalMs: Int64 = AutoBackup.minIntervalMs
    ) -> Verdict {
        guard state.dirtySinceMs != nil else { return .clean }
        if state.lastUploadAtMs == 0 { return .due }
        let elapsed = nowMs - state.lastUploadAtMs
        if elapsed < 0 { return .due }
        return elapsed < minIntervalMs ? .throttled : .due
    }

    /// The state after a change worth backing up. Idempotent: the FIRST such
    /// change sets the clock, so the throttle is measured from the last upload
    /// and not pushed out by every later edit.
    public static func markedDirty(_ state: State, nowMs: Int64) -> State {
        guard state.dirtySinceMs == nil else { return state }
        var next = state
        next.dirtySinceMs = nowMs
        return next
    }

    /// The state after a successful upload: the clock restarts and the account
    /// is clean again. The group key survives - it describes what was reported,
    /// not what was uploaded.
    public static func markedUploaded(_ state: State, nowMs: Int64) -> State {
        var next = state
        next.lastUploadAtMs = nowMs
        next.dirtySinceMs = nil
        return next
    }

    /// The canonical form of a group set.
    ///
    /// Sorted, because the order a repository hands its threads back in is not
    /// guaranteed stable and an unsorted comparison would report a change
    /// whenever the list was merely reordered.
    public static func groupsKey(_ groupIds: [String]) -> String {
        groupIds.sorted().joined(separator: ",")
    }

    /// The canonical form of a persisted synced state: the SHA-256 of its
    /// bytes, hex. The bytes themselves (drafts among them) never reach the
    /// `UserDefaults` row.
    public static func syncedStateKey(_ bytes: Data) -> String {
        SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
    }
}

// MARK: - Persistence

/// `UserDefaults`, keyed per account.
///
/// Two integers and a short string - no phrase, no key, no envelope - so this
/// needs neither the Keychain nor the device key.
public enum AutoBackupStore {

    private static func key(_ accountIdB64: String) -> String {
        "lakko.backup.auto.\(accountIdB64)"
    }

    public static func load(
        accountIdB64: String,
        defaults: UserDefaults = .standard
    ) -> AutoBackup.State {
        guard let data = defaults.data(forKey: key(accountIdB64)),
              let decoded = try? JSONDecoder().decode(AutoBackup.State.self, from: data)
        else {
            return .clean
        }
        return decoded
    }

    /// Not written once the account's session has ended
    /// (``AccountStoreGate``): the driver files an upload's outcome after
    /// the upload's wait, under the account it captured before it.
    public static func save(
        accountIdB64: String,
        state: AutoBackup.State,
        defaults: UserDefaults = .standard
    ) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        guard let data = try? JSONEncoder().encode(state) else { return }
        defaults.set(data, forKey: key(accountIdB64))
    }

    public static func clear(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: key(accountIdB64))
    }

    /// Record that something worth backing up has changed.
    public static func markDirty(
        accountIdB64: String,
        nowMs: Int64,
        defaults: UserDefaults = .standard
    ) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        let state = load(accountIdB64: accountIdB64, defaults: defaults)
        let next = AutoBackup.markedDirty(state, nowMs: nowMs)
        if next != state { save(accountIdB64: accountIdB64, state: next, defaults: defaults) }
    }

    /// Whether the set of groups this device knows about has changed since the
    /// last time this was asked, recording the new set as it goes.
    ///
    /// Persisted rather than held in memory, because an in-memory memo starts
    /// empty on every launch and would therefore report a change at every
    /// launch - turning "back up when membership moves" into "back up once per
    /// session", which is a different rule wearing the same name.
    public static func knownGroupsChanged(
        accountIdB64: String,
        groupIds: [String],
        defaults: UserDefaults = .standard
    ) -> Bool {
        // Sealed: nothing is recorded and no change is reported.
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return false }
        defer { gate.leave() }
        let key = AutoBackup.groupsKey(groupIds)
        var state = load(accountIdB64: accountIdB64, defaults: defaults)
        if state.lastGroupsKey == key { return false }
        state.lastGroupsKey = key
        save(accountIdB64: accountIdB64, state: state, defaults: defaults)
        return true
    }

    /// Whether the persisted synced state (ADR-0058: folders, flags, saved
    /// messages, the channel-subscription and admission-seed registers, and
    /// every other register the backup carries) has changed since the last
    /// time this was asked, recording the new digest as it goes.
    ///
    /// The one choke point for every synced-state change, whichever path
    /// wrote it (a local write, a sibling's envelope, a restore merge): the
    /// backup carries the whole state, so a change to any register is a
    /// change worth backing up. Read cursors and drafts count too; the
    /// minimum interval bounds how often that uploads.
    public static func syncedStateChanged(
        accountIdB64: String,
        bytes: Data,
        defaults: UserDefaults = .standard
    ) -> Bool {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return false }
        defer { gate.leave() }
        let key = AutoBackup.syncedStateKey(bytes)
        var state = load(accountIdB64: accountIdB64, defaults: defaults)
        if state.lastSyncedStateKey == key { return false }
        state.lastSyncedStateKey = key
        save(accountIdB64: accountIdB64, state: state, defaults: defaults)
        return true
    }
}
