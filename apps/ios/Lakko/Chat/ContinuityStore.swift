// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0073 R.5 / R.7 - the facts about a moved conversation that are this
// device's alone and that nothing else records:
//
//   * WHEN this device learned an old group was continued elsewhere: the
//     start of the window in which an old GROUP's journal is still read;
//   * that the founder's re-send of its own messages into the successor of
//     a one-to-one RAN (`ResetRepair.messagesToResend`), so a second pass
//     never sends them twice;
//   * the first half of a member's device swap that landed in a commit of
//     its own, so the second half leaves no `Members changed` row either;
//   * that an old group's push subscription was retired here.
//
// iOS mirror of `apps/web/src/chat/continuity-store.ts`. `UserDefaults`, per
// account, group ids, account keys and timestamps only. Removed with the
// account's data.
public final class ContinuityStore {

    /// The row's key; the account id (standard base64) follows the prefix.
    static let prefix = "lakko.reset-continuity.v1."

    private let defaults: UserDefaults
    private let ownerAccountIdB64: String?

    public init(defaults: UserDefaults = .standard, ownerAccountIdB64: String? = nil) {
        self.defaults = defaults
        self.ownerAccountIdB64 = ownerAccountIdB64
    }

    private var storageKey: String? {
        (ownerAccountIdB64 ?? AcceptedContactsStore.boundAccountIdB64).map { Self.prefix + $0 }
    }

    /// The stored row. Every field is read leniently: a missing or malformed
    /// field is empty, a malformed value inside one is skipped.
    struct Row: Equatable {
        /// base64 old group id -> when this device learned it moved (ms).
        var moved: [String: Int64] = [:]
        /// base64 old group id -> the re-send into its successor ran.
        var resent: Set<String> = []
        /// base64 group id -> the people whose superseded devices left it
        /// and whose new devices have not arrived
        /// (`ResetRepair.deviceSwapAcrossCommits`): account -> since when (ms).
        var swapping: [String: [String: Int64]] = [:]
        /// base64 old group id -> its push subscription was retired here.
        var pushRetired: Set<String> = []
        /// base64 old group id -> the DS message id (in the OLD journal) of
        /// the last message whose re-send into the successor landed: where an
        /// interrupted re-send continues.
        var resendCursor: [String: Int64] = [:]
    }

    private static func millis(_ raw: Any) -> Int64? {
        guard let number = raw as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID() else { return nil }
        let value = number.doubleValue
        guard value.isFinite, abs(value) < 9.0e18 else { return nil }
        return Int64(value)
    }

    private static func stamps(_ raw: Any?) -> [String: Int64] {
        guard let object = raw as? [String: Any] else { return [:] }
        var out: [String: Int64] = [:]
        for (key, value) in object {
            if let at = millis(value) { out[key] = at }
        }
        return out
    }

    private static func marks(_ raw: Any?) -> Set<String> {
        guard let object = raw as? [String: Any] else { return [] }
        var out: Set<String> = []
        for (key, value) in object {
            if let number = value as? NSNumber, CFGetTypeID(number) == CFBooleanGetTypeID(), number.boolValue {
                out.insert(key)
            }
        }
        return out
    }

    static func decode(_ data: Data) -> Row {
        guard let parsed = try? JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed]),
              let object = parsed as? [String: Any]
        else { return Row() }
        var swapping: [String: [String: Int64]] = [:]
        if let groups = object["swapping"] as? [String: Any] {
            for (groupIdB64, held) in groups {
                let people = stamps(held)
                if !people.isEmpty { swapping[groupIdB64] = people }
            }
        }
        return Row(
            moved: stamps(object["moved"]), resent: marks(object["resent"]),
            swapping: swapping, pushRetired: marks(object["pushRetired"]),
            resendCursor: stamps(object["resendCursor"]).filter { $0.value > 0 })
    }

    static func encode(_ row: Row) -> Data? {
        func numbers(_ stamps: [String: Int64]) -> [String: Any] {
            stamps.mapValues { NSNumber(value: $0) as Any }
        }
        func trues(_ marks: Set<String>) -> [String: Any] {
            Dictionary(uniqueKeysWithValues: marks.map { ($0, NSNumber(value: true) as Any) })
        }
        let payload: [String: Any] = [
            "moved": numbers(row.moved),
            "resent": trues(row.resent),
            "swapping": row.swapping.mapValues { numbers($0) as Any },
            "pushRetired": trues(row.pushRetired),
            "resendCursor": numbers(row.resendCursor),
        ]
        return try? JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])
    }

    private func load() -> Row {
        guard let storageKey, let raw = defaults.data(forKey: storageKey) else { return Row() }
        return Self.decode(raw)
    }

    private func save(_ row: Row) {
        // With no account bound nothing is kept: the facts are per call then.
        guard let storageKey, let raw = Self.encode(row) else { return }
        guard let gate = AccountStoreGate.enterUnlessWiped(
            ownerAccountIdB64 ?? AcceptedContactsStore.boundAccountIdB64 ?? "") else { return }
        defer { gate.leave() }
        defaults.set(raw, forKey: storageKey)
    }

    /// Record that `oldGroupIdB64` moved; the FIRST time wins.
    public func noteConversationMoved(oldGroupIdB64: String, nowMs: Int64) {
        var row = load()
        if row.moved[oldGroupIdB64] != nil { return }
        row.moved[oldGroupIdB64] = nowMs
        save(row)
    }

    /// When this device learned `oldGroupIdB64` moved, or `nil`.
    public func conversationMovedAt(oldGroupIdB64: String) -> Int64? {
        load().moved[oldGroupIdB64]
    }

    /// The re-send into the successor of `oldGroupIdB64` ran.
    public func markResent(oldGroupIdB64: String) {
        var row = load()
        row.resent.insert(oldGroupIdB64)
        save(row)
    }

    public func wasResent(oldGroupIdB64: String) -> Bool {
        load().resent.contains(oldGroupIdB64)
    }

    /// The old journal's message id of the last message whose re-send into
    /// the successor of `oldGroupIdB64` landed, or `nil` before the first.
    public func resendCursor(oldGroupIdB64: String) -> Int64? {
        load().resendCursor[oldGroupIdB64]
    }

    /// Advance that cursor. Never moves backward: a re-send is in the old
    /// journal's order, and a smaller id would send a landed message again.
    public func advanceResendCursor(oldGroupIdB64: String, to messageId: Int64) {
        var row = load()
        guard messageId > (row.resendCursor[oldGroupIdB64] ?? 0) else { return }
        row.resendCursor[oldGroupIdB64] = messageId
        save(row)
    }

    /// The old group's push subscription was retired on this device.
    public func markPushRetired(oldGroupIdB64: String) {
        var row = load()
        if row.pushRetired.contains(oldGroupIdB64) { return }
        row.pushRetired.insert(oldGroupIdB64)
        save(row)
    }

    public func wasPushRetired(oldGroupIdB64: String) -> Bool {
        load().pushRetired.contains(oldGroupIdB64)
    }

    /// The first halves of a device swap this device saw in `groupIdB64`:
    /// person key -> since when (ms).
    public func awaitedSwaps(groupIdB64: String) -> [String: Int64] {
        load().swapping[groupIdB64] ?? [:]
    }

    /// Replace that memory (an empty one removes the group's entry).
    public func setAwaitedSwaps(groupIdB64: String, _ awaited: [String: Int64]) {
        var row = load()
        let had = row.swapping[groupIdB64]
        if awaited.isEmpty {
            if had == nil { return }
            row.swapping.removeValue(forKey: groupIdB64)
        } else {
            if had == awaited { return }
            row.swapping[groupIdB64] = awaited
        }
        save(row)
    }

    /// Forget everything for the account (its data is wiped).
    public static func clear(ownerAccountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: prefix + ownerAccountIdB64)
    }
}
