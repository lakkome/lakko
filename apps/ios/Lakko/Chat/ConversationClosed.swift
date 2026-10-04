// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0065 phase 2 — a conversation this device CLOSED, as distinct from one
// its membership ENDED in (`MembershipEnded.swift`). `docs/ux/identity-recovery.md`
// section 16.4 (the one-to-one with a contact whose keys were reset) and 16.5
// (a group an admin closed after recreating it).
//
// The two are different facts. An ended membership is the core's own verdict
// (`Group.isActive()` is false, nothing can be decrypted again). A closed
// conversation is still a live MLS group the device is in and can read: the
// client refuses to SEND into it and renders the ended-chat shape with its own
// words, because the reason it stopped is a reason of the person's, not the
// protocol's. Keeping the two apart is what lets a closed group keep its live
// roster and its `Leave group` while a closed one-to-one drops every late row.

/// Why a conversation is closed on this device, and what the shape needs.
public enum ConversationClosedReason: Codable, Equatable {
    /// 16.4 — the other person reset their account keys. `accountIdB64` names
    /// them for the copy (through the contact record, by account id).
    case contactKeysReset(accountIdB64: String, closedAtMs: Int64)
    /// 16.5 — an admin closed the group after the owner's keys were reset.
    /// `closedByKeyB64` is the admin's account key resolved at receipt (or this
    /// device's own, when it did the closing); `successorGroupIdHex` the new
    /// group's journal id as the admin claims it, `nil` when closed without one.
    case groupClosed(closedByKeyB64: String, closedAtMs: Int64, successorGroupIdHex: String?)

    public var closedAtMs: Int64 {
        switch self {
        case .contactKeysReset(_, let at): return at
        case .groupClosed(_, let at, _): return at
        }
    }
}

/// Per-account map `groupIdHex -> reason` in `UserDefaults` (the
/// ``GroupNameStore`` shape): a small non-secret record of a decision this
/// device took or received, namespaced by account.
public final class ConversationClosedStore {

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func load(accountIdB64: String, groupIdHex: String) -> ConversationClosedReason? {
        guard let raw = map(accountIdB64: accountIdB64)[groupIdHex] else { return nil }
        return try? JSONDecoder().decode(ConversationClosedReason.self, from: raw)
    }

    public func all(accountIdB64: String) -> [String: ConversationClosedReason] {
        var out: [String: ConversationClosedReason] = [:]
        for (hex, raw) in map(accountIdB64: accountIdB64) {
            if let reason = try? JSONDecoder().decode(ConversationClosedReason.self, from: raw) {
                out[hex] = reason
            }
        }
        return out
    }

    public func record(accountIdB64: String, groupIdHex: String, reason: ConversationClosedReason) {
        guard let raw = try? JSONEncoder().encode(reason) else { return }
        var current = map(accountIdB64: accountIdB64)
        current[groupIdHex] = raw
        defaults.set(current, forKey: Self.key(accountIdB64))
    }

    public func remove(accountIdB64: String, groupIdHex: String) {
        var current = map(accountIdB64: accountIdB64)
        current[groupIdHex] = nil
        defaults.set(current, forKey: Self.key(accountIdB64))
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64))
    }

    private func map(accountIdB64: String) -> [String: Data] {
        defaults.dictionary(forKey: Self.key(accountIdB64)) as? [String: Data] ?? [:]
    }

    static func key(_ accountIdB64: String) -> String {
        "conversation_closed.\(accountIdB64)"
    }
}

// MARK: - 16.5 the group-closed envelope's receive gate

/// What the receive path does with one incoming group-closed envelope. The
/// ``GroupNameGate`` shape, which the envelope follows exactly (16.8): the
/// identical case decided before authority, the authority the receiver's,
/// then the strictly-greater order.
public enum GroupClosedVerdict: Equatable {
    /// The same stamp and successor as what is stored. Nothing to write.
    case acceptIdentical
    /// Authorised and strictly newer: store it, the group takes the closed
    /// shape, the later stamp wins the `Open the new group` target.
    case accept
    /// A plain member, or a group with no role table — "a notice from a plain
    /// member is dropped", and a table-less group has no admin to accept
    /// from. Dropped (invariant #4).
    case rejectNotAuthorized
    /// Authorised, but not newer than what is stored (a growth re-send, or
    /// the earlier of two admins' notices).
    case rejectStale
    /// The roles or the roster could not be read, or the sender leaf is not
    /// in the roster. Dropped, fail-closed.
    case rejectUnverifiable
}

/// The stored half the gate orders against.
public struct StoredGroupClosed: Equatable {
    public let closedAtMs: UInt64
    public let successorGroupIdHex: String?

    public init(closedAtMs: UInt64, successorGroupIdHex: String?) {
        self.closedAtMs = closedAtMs
        self.successorGroupIdHex = successorGroupIdHex
    }
}

public enum GroupClosedGate {

    public static func classify(
        incomingClosedAtMs: UInt64,
        incomingSuccessorGroupIdHex: String?,
        stored: StoredGroupClosed?,
        senderKey: Data?,
        authority: PushGroupRouting.SecretAuthority,
        isNewer: (_ incoming: UInt64, _ stored: UInt64) -> Bool = {
            groupClosedIsNewer(incomingClosedAtMs: $0, storedClosedAtMs: $1)
        }
    ) -> GroupClosedVerdict {
        // (1) Idempotent first: a re-send of what is stored changes nothing
        // and needs no authority.
        if let stored, stored.closedAtMs == incomingClosedAtMs,
           stored.successorGroupIdHex == incomingSuccessorGroupIdHex {
            return .acceptIdentical
        }
        // (2) Authority — the receiver's check. Unlike the group name, a
        // table-less group is NOT open: the sentence the row renders names an
        // admin who closed it, and such a group has none.
        switch authority {
        case .noRoleTable:
            return .rejectNotAuthorized
        case .unreadable:
            return .rejectUnverifiable
        case .roleTable(let ownerKey, let admins):
            guard let senderKey, !senderKey.isEmpty else { return .rejectUnverifiable }
            guard senderKey == ownerKey || admins.contains(senderKey) else {
                return .rejectNotAuthorized
            }
        }
        // (3) The first notice.
        guard let stored else { return .accept }
        // (4) Strictly greater, through the core.
        return isNewer(incomingClosedAtMs, stored.closedAtMs) ? .accept : .rejectStale
    }
}

// MARK: - 16.3 the eviction bookkeeping

/// Per group, per flagged contact: whether the eviction of their old devices
/// was proposed, when, and whether it applied (16.8). Keyed on the PREVIOUS
/// key — the key the old leaves resolve to — with the account id beside it so
/// the system row can name the person after the contact moved to new keys.
public struct KeyResetEvictionRecord: Codable, Equatable {
    public let accountIdB64: String
    public var proposedAtMs: Int64?
    public var applied: Bool

    public init(accountIdB64: String, proposedAtMs: Int64?, applied: Bool) {
        self.accountIdB64 = accountIdB64
        self.proposedAtMs = proposedAtMs
        self.applied = applied
    }
}

/// Per-account store: `groupIdHex -> previousKeyB64 -> record`, plus the
/// account-wide memory of every key a reset retired (`previousKeyB64 ->
/// accountIdB64`), which outlives the contact's verification: an owned
/// group's notice (16.5) and a flagged roster row are about the OLD key,
/// which stays in the group's role table and roster for as long as it is
/// not removed.
public final class KeyResetEvictionStore {

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func records(accountIdB64: String, groupIdHex: String) -> [String: KeyResetEvictionRecord] {
        guard let raw = (defaults.dictionary(forKey: Self.key(accountIdB64)) as? [String: Data])?[groupIdHex],
              let decoded = try? JSONDecoder().decode([String: KeyResetEvictionRecord].self, from: raw)
        else { return [:] }
        return decoded
    }

    public func record(
        accountIdB64: String, groupIdHex: String, previousKeyB64: String,
        _ value: KeyResetEvictionRecord
    ) {
        var current = records(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        current[previousKeyB64] = value
        guard let raw = try? JSONEncoder().encode(current) else { return }
        var map = defaults.dictionary(forKey: Self.key(accountIdB64)) as? [String: Data] ?? [:]
        map[groupIdHex] = raw
        defaults.set(map, forKey: Self.key(accountIdB64))
    }

    public func removeGroup(accountIdB64: String, groupIdHex: String) {
        var map = defaults.dictionary(forKey: Self.key(accountIdB64)) as? [String: Data] ?? [:]
        map[groupIdHex] = nil
        defaults.set(map, forKey: Self.key(accountIdB64))
    }

    /// Every key a reset retired, with the account it belonged to.
    public func retiredKeys(accountIdB64: String) -> [String: String] {
        defaults.dictionary(forKey: Self.retiredKey(accountIdB64)) as? [String: String] ?? [:]
    }

    public func rememberRetired(accountIdB64: String, previousKeyB64: String, contactAccountIdB64: String) {
        var current = retiredKeys(accountIdB64: accountIdB64)
        current[previousKeyB64] = contactAccountIdB64
        defaults.set(current, forKey: Self.retiredKey(accountIdB64))
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64))
        defaults.removeObject(forKey: Self.retiredKey(accountIdB64))
    }

    static func key(_ accountIdB64: String) -> String {
        "keys_reset_eviction.\(accountIdB64)"
    }

    static func retiredKey(_ accountIdB64: String) -> String {
        "keys_reset_retired.\(accountIdB64)"
    }
}

// MARK: - 16.3 the system row

/// The timeline row written when the sweep that removed a contact's old
/// devices lands, on a device that knows the reset. A machine marker in the
/// shape ``CallTranscriptRow`` uses, parsed and worded at render time so the
/// sentence follows the interface language and names the contact by account
/// id (the key the row is about is retired, and the contact may have moved to
/// new keys since).
public enum KeyResetSystemRow {
    public static let markerPrefix = "[keys-reset-removed]"

    public static func marker(accountIdB64: String) -> String {
        "\(markerPrefix) \(accountIdB64)"
    }

    /// The account id a stored marker names, or `nil` for any other row.
    public static func parse(_ text: String) -> String? {
        let fields = text.split(separator: " ", maxSplits: 1).map(String.init)
        guard fields.count == 2, fields[0] == markerPrefix, !fields[1].isEmpty else { return nil }
        return fields[1]
    }

    /// The sentence, in the interface language of the moment.
    public static func sentence(name: String?) -> String {
        Localization.text(
            "chat.keys_reset.system_removed",
            texts: ["name": name ?? ResolvedDisplayName.placeholderSomeone])
    }
}

// MARK: - The copy the closed shapes render (16.4 / 16.5)

/// The chat-list row's second line for a closed conversation.
public func closedListLine(_ reason: ConversationClosedReason, contactName: String?) -> String {
    switch reason {
    case .contactKeysReset:
        return Localization.text(
            "chat.keys_reset.one_to_one_row", texts: ["name": contactName ?? ResolvedDisplayName.placeholderSomeone])
    case .groupClosed:
        return Localization.text("group.closed.row")
    }
}

/// The terminal timeline row of a closed conversation: its own first line,
/// the standing consequence (`chat.ended.consequence`) second.
public func closedTerminalRow(_ reason: ConversationClosedReason, name: String?) -> TerminalRowCopy {
    let headline: String
    switch reason {
    case .contactKeysReset:
        headline = Localization.text(
            "chat.keys_reset.one_to_one_closed", texts: ["name": name ?? ResolvedDisplayName.placeholderSomeone])
    case .groupClosed:
        headline = Localization.text(
            "group.closed.system", texts: ["name": name ?? ResolvedDisplayName.placeholderSomeone])
    }
    return TerminalRowCopy(headline: headline, consequence: endedConsequenceMessage)
}

/// What occupies the composer's slot of a closed conversation: the static bar
/// and one button, whose availability is the caller's (16.4: enabled once the
/// contact is verified; 16.5: shown only once this device joined the new
/// group).
public struct ClosedComposerSlot: Equatable {
    public let notice: String
    /// The button's label, or `nil` when no button is shown at all.
    public let action: String?
    public let actionEnabled: Bool
    /// 16.4 — the helper under a disabled `Start a new chat`, with the link
    /// `Open contact` beside it.
    public let helper: String?
    public let draft: String?
    public let draftLabel: String?
}

public func closedComposerSlot(
    _ reason: ConversationClosedReason,
    draft: String,
    contactName: String?,
    contactVerified: Bool,
    joinedSuccessor: Bool
) -> ClosedComposerSlot {
    let worthKeeping = !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    switch reason {
    case .contactKeysReset:
        return ClosedComposerSlot(
            notice: Localization.text("chat.keys_reset.one_to_one_composer"),
            action: Localization.text("chat.keys_reset.start_new_chat"),
            actionEnabled: contactVerified,
            helper: contactVerified
                ? nil
                : (contactName.map {
                    Localization.text("contacts.keys_reset.verify_first", texts: ["name": $0])
                } ?? Localization.text("contacts.keys_reset.verify_first_short")),
            draft: worthKeeping ? draft : nil,
            draftLabel: worthKeeping ? unsentDraftLabel : nil)
    case .groupClosed(_, _, let successor):
        return ClosedComposerSlot(
            notice: Localization.text("group.closed.composer"),
            action: successor != nil && joinedSuccessor ? Localization.text("group.closed.open_new") : nil,
            actionEnabled: successor != nil && joinedSuccessor,
            helper: nil,
            draft: worthKeeping ? draft : nil,
            draftLabel: worthKeeping ? unsentDraftLabel : nil)
    }
}
