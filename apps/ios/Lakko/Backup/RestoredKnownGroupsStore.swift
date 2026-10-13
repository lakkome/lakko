// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The group ids a restored backup named that this device has not joined.
///
/// The account backup carries no MLS state: a restored device is admitted to
/// its chats by their other members (ADR-0050 section 2.5a), never from the
/// backup. What the backup does carry is the LIST of the account's
/// conversations. A device that has just restored holds none of them, so its
/// own first upload would otherwise replace that list with an empty one. The
/// ids the restored backup named are kept here, and every backup this device
/// uploads carries its own groups plus the ids here it has not joined yet
/// (``uploadGroupIds(own:restored:)``).
///
/// An id leaves the set when this device holds the group (a join, or a chat
/// bootstrap that finds it already held), when the user deletes that chat, and
/// when the local group ends (removed / left).
///
/// This is NOT the pairing bundle's list (``StateBundle/loadKnownGroupIds(accountIdB64:defaults:)``):
/// nothing here feeds the chat list's "Synchronizing..." label, and nothing
/// here is read to rebuild a `Group` handle (``KnownGroupsStore`` holds the
/// groups this device has).
///
/// Per account, in `UserDefaults`: the ids are non-secret (the Delivery
/// Service sees every one of them as a journal name). Removed by
/// ``AccountLocalDataWipe`` (Forget device, Delete account); kept on Log out.
public enum RestoredKnownGroupsStore {

    static let keyPrefix = "lakko.backup.restored-known-groups.v1."

    static func key(accountIdB64: String) -> String {
        keyPrefix + accountIdB64
    }

    /// The restored ids for `accountIdB64`, lowercase hex. Empty when none.
    public static func load(
        accountIdB64: String,
        defaults: UserDefaults = .standard
    ) -> Set<String> {
        Set(defaults.stringArray(forKey: key(accountIdB64: accountIdB64)) ?? [])
    }

    /// Add the ids a restored backup named. Additive: a second restore never
    /// drops what an earlier one recorded. Anything that is not a 32-byte id
    /// in hex is ignored.
    public static func add(
        accountIdB64: String,
        groupIdsHex: [String],
        defaults: UserDefaults = .standard
    ) {
        let incoming = groupIdsHex.compactMap(normalized)
        guard !incoming.isEmpty else { return }
        var current = load(accountIdB64: accountIdB64, defaults: defaults)
        let before = current.count
        current.formUnion(incoming)
        if current.count == before { return }
        save(current, accountIdB64: accountIdB64, defaults: defaults)
    }

    /// Drop one id: the device joined that group, the user deleted that chat,
    /// or the local group ended. No-op when absent.
    public static func remove(
        accountIdB64: String,
        groupIdHex: String,
        defaults: UserDefaults = .standard
    ) {
        prune(accountIdB64: accountIdB64, heldGroupIdsHex: [groupIdHex], defaults: defaults)
    }

    /// Drop every id in `heldGroupIdsHex`: the groups this device holds.
    public static func prune(
        accountIdB64: String,
        heldGroupIdsHex: [String],
        defaults: UserDefaults = .standard
    ) {
        let current = load(accountIdB64: accountIdB64, defaults: defaults)
        guard !current.isEmpty else { return }
        let remaining = current.subtracting(heldGroupIdsHex.map { $0.lowercased() })
        if remaining.count == current.count { return }
        save(remaining, accountIdB64: accountIdB64, defaults: defaults)
    }

    /// Forget device / Delete account.
    public static func clear(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: key(accountIdB64: accountIdB64))
    }

    /// The group ids a backup upload carries: the groups this device holds
    /// (`own`) and the restored ids it has not joined. Sorted, without
    /// duplicates, lowercase hex.
    public static func uploadGroupIds(own: [String], restored: Set<String>) -> [String] {
        Array(Set(own.map { $0.lowercased() }).union(restored)).sorted()
    }

    /// ``uploadGroupIds(own:restored:)`` over the stored set of `accountIdB64`.
    public static func uploadGroupIds(
        accountIdB64: String,
        own: [String],
        defaults: UserDefaults = .standard
    ) -> [String] {
        uploadGroupIds(own: own, restored: load(accountIdB64: accountIdB64, defaults: defaults))
    }

    private static func save(_ ids: Set<String>, accountIdB64: String, defaults: UserDefaults) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        let key = key(accountIdB64: accountIdB64)
        if ids.isEmpty {
            defaults.removeObject(forKey: key)
        } else {
            defaults.set(ids.sorted(), forKey: key)
        }
    }

    /// Lowercase hex of a 32-byte id, or `nil`.
    private static func normalized(_ hex: String) -> String? {
        let lower = hex.lowercased()
        guard lower.count == 64, lower.allSatisfy({ $0.isHexDigit }) else { return nil }
        return lower
    }
}
