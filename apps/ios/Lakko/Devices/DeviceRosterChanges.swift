// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation

extension Notification.Name {
    /// The account's device list changed on this device and the persisted
    /// DeviceGroup snapshot holds the change. `userInfo["accountIdB64"]` names
    /// the account.
    static let lakkoDeviceRosterChanged = Notification.Name("lakko.deviceRosterChanged")
}

/// "The list follows the account" (`docs/ux/second-device.md` section 3): an
/// open Devices screen reads the list again when it changes, on every route.
/// The screen reads the list off the persisted snapshot, so this is said only
/// once that snapshot holds the change.
enum DeviceRosterChanges {

    static func note(accountIdB64: String) {
        NotificationCenter.default.post(
            name: .lakkoDeviceRosterChanged, object: nil,
            userInfo: ["accountIdB64": accountIdB64])
    }

    /// Every change of `accountIdB64`'s list, on the next turn of the main
    /// queue: a reader never runs inside the path that posted (a commit under
    /// its account lock).
    static func publisher(accountIdB64: String) -> AnyPublisher<Void, Never> {
        NotificationCenter.default.publisher(for: .lakkoDeviceRosterChanged)
            .filter { ($0.userInfo?["accountIdB64"] as? String) == accountIdB64 }
            .map { _ in () }
            .receive(on: DispatchQueue.main)
            .eraseToAnyPublisher()
    }
}

/// The commit a poll applied reaches the disk only with the batch's persist,
/// and a failed persist is written again with the next batch: the change is
/// said after the write that holds it, not after the commit.
struct RosterChangeAfterPersist {
    private var owed = false

    /// A commit was applied in memory.
    mutating func commitApplied() {
        owed = true
    }

    /// The handle's snapshot reached the disk. `true` when a change was owed.
    mutating func persisted() -> Bool {
        defer { owed = false }
        return owed
    }
}
