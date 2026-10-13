// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0073 "Phase 2 revised", R.6 - the per-`(account, group)` CLAIM state
// a received `group_closed` envelope leaves on this device.
//
// iOS mirror of `apps/web/src/chat/group-closed-store.ts` (the state, its
// transitions and its row codec) with `group-closed-idb.ts` replaced by
// `UserDefaults`.
//
// Since the visible flow was removed, a `group_closed` envelope has one
// producer: a device founding the successor of a conversation that cannot
// be repaired in place (a one-to-one whose other person reset their keys, a
// group whose owner did). The decision that guards it is
// `SuccessorClaimRules.claimVerdict`: the FIRST eligible claim in the old
// journal's order wins, for good. This file is what is kept between
// sessions:
//
//   * `accepted` - the winning claim: the conversation's successor;
//   * `pending`  - claims that arrived before this device had its own
//     verdict on the reset, kept in journal order and judged again after
//     the identity check the claim itself triggers;
//   * `own`      - this device's own claim, from the moment it is posted
//     until step 3 decided it (so a restart between the post and the read
//     neither founds a second successor nor forgets the local one).
//
// Nothing here is shown: the store has no reader in any view (R.0).

/// This device's own claim, posted and not yet decided.
public struct OwnClaim: Codable, Equatable, Sendable {
    /// base64 of the successor this device created locally.
    public let successorGroupIdB64: String
    /// The DS message id the publish returned; `nil` while the publish is in
    /// flight (a restart then re-reads the journal for its own row).
    public let messageId: Int64?

    public init(successorGroupIdB64: String, messageId: Int64?) {
        self.successorGroupIdB64 = successorGroupIdB64
        self.messageId = messageId
    }
}

/// The successor THIS device created for the old group (R.6 step 1), kept
/// from the moment it exists until its Welcome was delivered, and after that
/// as the mark "this device founded it" (the founder alone re-sends, R.7
/// case 1). The Welcome is a public MLS artefact sealed to its recipients;
/// it is kept so a restart between winning the claim and delivering it does
/// not leave a successor nobody can join.
public struct PreparedSuccessor: Codable, Equatable, Sendable {
    public let successorGroupIdB64: String
    /// base64 of the Welcome of the founding commit; `nil` once delivered.
    public let welcomeB64: String?
    /// base64 account keys of the people the Welcome goes to, one each.
    public let targetKeysB64: [String]
    public let delivered: Bool

    public init(successorGroupIdB64: String, welcomeB64: String?, targetKeysB64: [String], delivered: Bool) {
        self.successorGroupIdB64 = successorGroupIdB64
        self.welcomeB64 = welcomeB64
        self.targetKeysB64 = targetKeysB64
        self.delivered = delivered
    }
}

/// What is stored for one old group.
public struct StoredGroupClosed: Codable, Equatable, Sendable {
    public let accepted: SuccessorClaim?
    public let pending: [SuccessorClaim]
    public let own: OwnClaim?
    public let prepared: PreparedSuccessor?

    public init(
        accepted: SuccessorClaim?, pending: [SuccessorClaim], own: OwnClaim?,
        prepared: PreparedSuccessor? = nil
    ) {
        self.accepted = accepted
        self.pending = pending
        self.own = own
        self.prepared = prepared
    }

    /// The empty state.
    public static let noClaims = StoredGroupClosed(accepted: nil, pending: [], own: nil)
}

/// The pure transitions of the claim state and its row codec.
public enum GroupClosedRules {

    /// At most this many undecided claims are kept per group: the journal's
    /// claims arrive from members, and an unbounded list would be a way to
    /// grow this device's storage. Chosen, not measured.
    public static let maxPendingClaims = 16

    /// `state` with `claim` kept for a later verdict, in journal order, once.
    public static func withPendingClaim(_ state: StoredGroupClosed, _ claim: SuccessorClaim) -> StoredGroupClosed {
        if state.pending.contains(where: { $0.messageId == claim.messageId }) { return state }
        let pending = Array(
            SuccessorClaimRules.inJournalOrder(state.pending + [claim]).prefix(maxPendingClaims))
        return StoredGroupClosed(
            accepted: state.accepted, pending: pending, own: state.own, prepared: state.prepared)
    }

    /// `state` with `claim` as the conversation's successor; the pending
    /// list is spent.
    public static func withAcceptedClaim(_ state: StoredGroupClosed, _ claim: SuccessorClaim) -> StoredGroupClosed {
        StoredGroupClosed(accepted: claim, pending: [], own: state.own, prepared: state.prepared)
    }

    // MARK: - Row codec

    /// Schema version of the JSON payload. Version 3 is the claim state;
    /// rows of the visible flow (1, 2) are not read.
    public static let rowVersion = 3

    /// The largest integer JSON carries exactly on every client (2^53 - 1):
    /// a message id above it is not one the web twin could have written.
    private static let maxSafeInteger: Double = 9_007_199_254_740_991

    private static func claimToJson(_ c: SuccessorClaim) -> [String: Any] {
        [
            "m": NSNumber(value: c.messageId),
            "s": c.successorGroupIdB64,
            "k": c.senderKeyB64,
            "a": NSNumber(value: c.senderWasAdmin),
            "t": c.adminKeysAtReceiptB64,
        ]
    }

    /// A JSON number that is a safe integer, or `nil` (a boolean is not a
    /// number here, as it is not in the web twin).
    private static func safeInteger(_ raw: Any?) -> Int64? {
        guard let number = raw as? NSNumber, !isBoolean(number) else { return nil }
        let value = number.doubleValue
        guard value.isFinite, value == value.rounded(.towardZero), abs(value) <= maxSafeInteger else {
            return nil
        }
        return Int64(value)
    }

    private static func isBoolean(_ number: NSNumber) -> Bool {
        CFGetTypeID(number) == CFBooleanGetTypeID()
    }

    private static func boolean(_ raw: Any?) -> Bool? {
        guard let number = raw as? NSNumber, isBoolean(number) else { return nil }
        return number.boolValue
    }

    private static func strings(_ raw: Any?) -> [String]? {
        guard let items = raw as? [Any] else { return nil }
        var out: [String] = []
        for item in items {
            guard let text = item as? String else { return nil }
            out.append(text)
        }
        return out
    }

    private static func claimFromJson(_ raw: Any?) -> SuccessorClaim? {
        guard let r = raw as? [String: Any] else { return nil }
        guard let m = safeInteger(r["m"]), m >= 0 else { return nil }
        guard let s = r["s"] as? String, let k = r["k"] as? String, let a = boolean(r["a"]) else { return nil }
        guard let t = strings(r["t"]) else { return nil }
        return SuccessorClaim(
            messageId: m, successorGroupIdB64: s, senderKeyB64: k, senderWasAdmin: a,
            adminKeysAtReceiptB64: t)
    }

    /// Encode the rows to the payload JSON (the shape the web twin writes:
    /// `{"v":3,"g":{<group>:{"a","p","o","r"?}}}`).
    public static func encodeRow(_ rows: [String: StoredGroupClosed]) -> String {
        var groups: [String: Any] = [:]
        for (groupIdB64, value) in rows {
            var entry: [String: Any] = [
                "a": value.accepted.map { claimToJson($0) as Any } ?? NSNull(),
                "p": value.pending.map(claimToJson),
                "o": value.own.map { own -> Any in
                    [
                        "s": own.successorGroupIdB64,
                        "m": own.messageId.map { NSNumber(value: $0) as Any } ?? NSNull(),
                    ] as [String: Any]
                } ?? NSNull(),
            ]
            if let prepared = value.prepared {
                entry["r"] = [
                    "s": prepared.successorGroupIdB64,
                    "w": prepared.welcomeB64.map { $0 as Any } ?? NSNull(),
                    "t": prepared.targetKeysB64,
                    "d": NSNumber(value: prepared.delivered),
                ] as [String: Any]
            }
            groups[groupIdB64] = entry
        }
        let payload: [String: Any] = ["v": NSNumber(value: rowVersion), "g": groups]
        guard let data = try? JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys]),
              let json = String(data: data, encoding: .utf8)
        else {
            // Unreachable for the value types above; an empty row set is the
            // fail-closed answer (nothing is judged from it).
            return "{\"g\":{},\"v\":\(rowVersion)}"
        }
        return json
    }

    /// Decode payload JSON, dropping any malformed entry rather than
    /// trusting it. `nil` for a payload that is not this schema at all.
    public static func decodeRow(_ json: String) -> [String: StoredGroupClosed]? {
        guard let data = json.data(using: .utf8),
              let parsed = try? JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed]),
              let obj = parsed as? [String: Any]
        else { return nil }
        guard let version = safeInteger(obj["v"]), version == Int64(rowVersion) else { return nil }
        var rows: [String: StoredGroupClosed] = [:]
        guard let groups = obj["g"] as? [String: Any] else { return rows }
        for (groupIdB64, rawEntry) in groups {
            guard let raw = rawEntry as? [String: Any] else { continue }
            let a = raw["a"]
            let aAbsent = a == nil || a is NSNull
            let accepted = aAbsent ? nil : claimFromJson(a)
            if !aAbsent && accepted == nil { continue }
            var pending: [SuccessorClaim] = []
            if let items = raw["p"] as? [Any] {
                for item in items {
                    if let claim = claimFromJson(item) { pending.append(claim) }
                }
            }
            var own: OwnClaim?
            if let o = raw["o"] as? [String: Any], let os = o["s"] as? String {
                let om = o["m"]
                if om is NSNull {
                    own = OwnClaim(successorGroupIdB64: os, messageId: nil)
                } else if let id = safeInteger(om) {
                    own = OwnClaim(successorGroupIdB64: os, messageId: id)
                }
            }
            var prepared: PreparedSuccessor?
            if let r = raw["r"] as? [String: Any],
               let rs = r["s"] as? String,
               let rt = strings(r["t"]),
               let rd = boolean(r["d"]) {
                let rw = r["w"]
                if rw is NSNull {
                    prepared = PreparedSuccessor(
                        successorGroupIdB64: rs, welcomeB64: nil, targetKeysB64: rt, delivered: rd)
                } else if let welcome = rw as? String {
                    prepared = PreparedSuccessor(
                        successorGroupIdB64: rs, welcomeB64: welcome, targetKeysB64: rt, delivered: rd)
                }
            }
            rows[groupIdB64] = StoredGroupClosed(
                accepted: accepted,
                pending: Array(SuccessorClaimRules.inJournalOrder(pending).prefix(maxPendingClaims)),
                own: own,
                prepared: prepared)
        }
        return rows
    }
}

/// The claim states of one signed-in account, in `UserDefaults`: one blob per
/// `(owner account id, old group id)`, the row codec above with a single
/// entry, SEALED under the device key before it is written.
///
/// A row can hold a prepared Welcome (R.6 step 1): the founding commit's
/// Welcome of a successor nobody was told about yet. It is sealed to its
/// recipients, but it also names who they are and which group continues
/// which, and web keeps the same row under its device key; so it is never
/// written as it is. Every write goes through the `Sealer` the caller
/// supplies (the Keychain wrap the message log and the held-send queue use);
/// a failed seal writes nothing, and a blob that does not open, or a row
/// written before the store was sealed, reads as "no claim state": nothing
/// is judged from it.
///
/// Decisions are the caller's (`SuccessorClaimRules`); this only stores what
/// it is told to. Removed with the account's data.
public final class GroupClosedStore {

    /// `v3`: the sealed rows. `v2` rows were plaintext JSON strings; they are
    /// never read, and are removed with the account's data.
    static let prefix = "lakko.group-closed.v3."
    static let plaintextPrefix = "lakko.group-closed.v2."

    /// How a row is protected at rest: the same shape the held-send queue
    /// takes.
    public typealias Sealer = HeldSendQueue.Sealer

    private let defaults: UserDefaults
    private let ownerAccountIdB64: String?
    private let sealer: Sealer

    public init(
        defaults: UserDefaults = .standard, ownerAccountIdB64: String? = nil, sealer: Sealer
    ) {
        self.defaults = defaults
        self.ownerAccountIdB64 = ownerAccountIdB64
        self.sealer = sealer
    }

    /// Standard base64 has no `.`, so the account part ends at the first one.
    private static func accountPrefix(_ ownerAccountIdB64: String) -> String {
        prefix + ownerAccountIdB64 + "."
    }

    private var accountPrefix: String? {
        (ownerAccountIdB64 ?? AcceptedContactsStore.boundAccountIdB64).map(Self.accountPrefix)
    }

    private func storageKey(_ groupIdB64: String) -> String? {
        accountPrefix.map { $0 + groupIdB64 }
    }

    /// The claim state of an old group, or `nil` when it has none. A blob
    /// that does not open or does not decode reads as none.
    public func read(groupIdB64: String) -> StoredGroupClosed? {
        guard let key = storageKey(groupIdB64), let sealed = defaults.data(forKey: key),
              let opened = try? sealer.open(sealed),
              let json = String(data: opened, encoding: .utf8)
        else { return nil }
        return GroupClosedRules.decodeRow(json)?[groupIdB64]
    }

    /// Replace the value for a group. Returns whether it was written (`false`
    /// with no account bound, or when the row could not be sealed: it is
    /// never stored unsealed).
    @discardableResult
    public func write(groupIdB64: String, _ value: StoredGroupClosed) -> Bool {
        guard let key = storageKey(groupIdB64) else { return false }
        let json = GroupClosedRules.encodeRow([groupIdB64: value])
        guard let sealed = try? sealer.seal(Data(json.utf8)) else { return false }
        defaults.set(sealed, forKey: key)
        return true
    }

    /// Forget a group's claim state (the `Delete this chat` path).
    public func drop(groupIdB64: String) {
        guard let key = storageKey(groupIdB64) else { return }
        defaults.removeObject(forKey: key)
    }

    /// Every stored `(groupIdB64, value)` of the account.
    public func entries() -> [String: StoredGroupClosed] {
        guard let accountPrefix else { return [:] }
        var out: [String: StoredGroupClosed] = [:]
        for key in defaults.dictionaryRepresentation().keys where key.hasPrefix(accountPrefix) {
            let groupIdB64 = String(key.dropFirst(accountPrefix.count))
            if let value = read(groupIdB64: groupIdB64) { out[groupIdB64] = value }
        }
        return out
    }

    /// Forget every claim state of the account (its data is wiped),
    /// plaintext rows of the earlier store included.
    public static func clear(ownerAccountIdB64: String, defaults: UserDefaults = .standard) {
        let sealedRows = accountPrefix(ownerAccountIdB64)
        let plaintextRows = plaintextPrefix + ownerAccountIdB64 + "."
        for key in defaults.dictionaryRepresentation().keys
        where key.hasPrefix(sealedRows) || key.hasPrefix(plaintextRows) {
            defaults.removeObject(forKey: key)
        }
    }
}
