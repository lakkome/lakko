// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// `docs/ux/group-and-channel-privacy.md` section 10 — creation counts as
/// activity, so a conversation nothing has been said in yet sits where it
/// arrived and not below everything else.
public enum GroupArrival {

    /// The time the conversation list sorts a chat or a group by: its last
    /// message's time, else the moment this device created or joined it.
    ///
    /// The arrival never beats a message. A device added to an account joins
    /// all its chats at once, and the history bundle gives those chats their
    /// real last message; taking the newer of the two would move every one of
    /// them to the moment of joining and lose the list's order. A person
    /// removed and added back has no messages left, so they still land at the
    /// top.
    public static func activityMs(lastMessageMs: Int64?, arrivedAtMs: Int64?) -> Int64? {
        lastMessageMs ?? arrivedAtMs
    }
}

/// When this device created or joined each group, per account. Written where a
/// group first comes to be held here, and again on a rejoin; a group reloaded
/// at launch keeps its time.
public final class GroupArrivalStore {

    private static let prefix = "lakko.group-arrival.v1."

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    private func key(_ accountIdB64: String) -> String { Self.prefix + accountIdB64 }

    /// Every stamp of the account, by group id hex.
    public func load(accountIdB64: String) -> [String: Int64] {
        guard let stored = defaults.dictionary(forKey: key(accountIdB64)) else { return [:] }
        return stored.compactMapValues { ($0 as? NSNumber)?.int64Value }
    }

    /// This device now holds `groupIdHex`, as of `atMs`.
    public func remember(accountIdB64: String, groupIdHex: String, atMs: Int64) {
        update(accountIdB64) { $0[groupIdHex] = atMs }
    }

    /// The group is gone from this device.
    public func remove(accountIdB64: String, groupIdHex: String) {
        update(accountIdB64) { $0[groupIdHex] = nil }
    }

    private func update(_ accountIdB64: String, _ change: (inout [String: Int64]) -> Void) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var stamps = load(accountIdB64: accountIdB64)
        change(&stamps)
        if stamps.isEmpty {
            defaults.removeObject(forKey: key(accountIdB64))
        } else {
            defaults.set(stamps.mapValues { NSNumber(value: $0) }, forKey: key(accountIdB64))
        }
    }

    /// The account's data goes from this device.
    public func clearForAccount(accountIdB64: String) {
        defaults.removeObject(forKey: key(accountIdB64))
    }
}
