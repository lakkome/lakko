// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Which conversation on this device is THE one-to-one with a contact.
///
/// Mirror of `apps/web/src/contacts/contact-conversation.ts`.
///
/// Two screens need it and must agree: Contacts (`Message` on a contact's
/// card) and the call screen's `Invite contacts`
/// (`docs/ux/call-links.md` section 3), which sends the invitation into that
/// conversation and founds it first when this device holds none.
///
/// ## The rule
///
/// 1. The conversation linked on the contact record, when the registry still
///    holds it AND it is a one-to-one with this contact, by the same test as
///    rule 2. A linked id whose chat is gone (deleted here, or never on this
///    device) is not an address: seen live on web 2026-09-26 as a contact
///    whose Message opened "Conversation not found". Nor is a linked id that
///    names a group, or a one-to-one with someone else: seen live on web
///    2026-11-05 as a contact whose Message opened a group the person is in.
///    A stored id is a hint, never the answer by itself.
/// 2. Else a conversation whose kind is one-to-one (ADR-0062) and whose
///    PEOPLE are exactly this account and the contact. People, not leaves: a
///    person is every one of their devices (ADR-0050), so a roster of three
///    leaves can be two people, and the contact is known by the account key.
///
/// A two-person GROUP is never the answer: it is a group that happens to hold
/// two people, and writing an invitation there is not writing to the contact.
///
/// Pure: the readers are passed in, so a host test can run it without a
/// group handle.
enum ContactConversation {

    /// - Parameters:
    ///   - contactKey: the contact's person (account) key.
    ///   - linkedId: the conversation id recorded on the contact, if any.
    ///   - conversations: every conversation this device holds.
    ///   - ownKey: this account's person key, or `nil` before a session.
    ///   - peopleOf: the distinct people of a conversation, by person key. A
    ///     torn handle reads as empty.
    ///   - isOneToOne: whether the conversation's kind is one-to-one.
    ///   - samePerson: whether two person keys are one person (a proven
    ///     rotation names one person by two keys).
    static func find<Conversation, Key: Equatable>(
        contactKey: Key,
        linkedId: String?,
        conversations: [Conversation],
        idOf: (Conversation) -> String,
        ownKey: Key?,
        peopleOf: (Conversation) -> [Key],
        isOneToOne: (Conversation) -> Bool,
        samePerson: (Key, Key) -> Bool = { $0 == $1 }
    ) -> Conversation? {
        // Two people, one of them the contact and the other this account. With
        // no session key yet only the linked conversation can answer, and only
        // when its other person is the contact.
        func isTheirs(_ conversation: Conversation) -> Bool {
            guard isOneToOne(conversation) else { return false }
            let people = peopleOf(conversation)
            guard people.count == 2,
                  let contactIdx = people.firstIndex(where: { samePerson($0, contactKey) })
            else { return false }
            guard let ownKey else { return true }
            return samePerson(people[1 - contactIdx], ownKey)
        }
        if let linkedId, let linked = conversations.first(where: { idOf($0) == linkedId }),
           isTheirs(linked) {
            return linked
        }
        guard ownKey != nil else { return nil }
        return conversations.first(where: isTheirs)
    }
}

/// One person, one one-to-one row.
///
/// Mirror of `apps/web/src/contacts/duplicate-one-to-one.ts`.
///
/// A device can hold two one-to-ones with the same person: it founded one
/// (A) whose Welcome the other person's device never opened, and later that
/// person founded another (B) in answer to a request this account sent (a link
/// answer, an Add). Both showed as rows in Chats (seen live 2026-11-07,
/// desktop and Pixel).
///
/// The other person founding B settles it: a device founds a one-to-one in
/// answer to a request only when it holds none live with the requester
/// (ADR-0079 section 5, the restore step), so A is dead on their side and B
/// is where they read. B is therefore recorded as A's successor in the
/// conversation lineage (ADR-0073 R.5): one row, whose head is B, with A's
/// history above it, and A's journal no longer polled.
///
/// Only a one-to-one founded by the PEER continues one founded by THIS
/// account, unless this device knows which of the two it held first
/// (``Held/heldSinceMs``, ``ConversationHeldSince``): then the newer
/// one-to-one with the person continues the older one whoever founded either
/// (base scenario 9, 2026-11-22): a person who lost their one-to-one and
/// founded a new one, or a one-to-one another device of this account founded
/// while this device held an older one. When the times are equal or unknown,
/// two one-to-ones founded by the same side are left as they are.
///
/// Pure: the caller reads the rosters, founders and lineage.
enum DuplicateOneToOne {
    /// A conversation group this device holds, as the rule reads it.
    struct Held: Equatable {
        /// Lower-case hex of the group id.
        let groupHex: String
        /// The conversation the lineage puts it in (its root's hex).
        let conversationHex: String
        /// Kind `chat` (ADR-0062).
        let isOneToOne: Bool
        /// The distinct people of the roster, base64 account keys.
        let people: [String]
        /// The founder's account key (base64), or `nil` when unreadable.
        let founderKeyB64: String?
        /// When this device first held the group, or `nil` when unknown.
        var heldSinceMs: Int64? = nil
    }

    /// Whether `a` was first held strictly after `b` on this device.
    static func heldAfter(_ a: Held, _ b: Held) -> Bool {
        guard let at = a.heldSinceMs, let bt = b.heldSinceMs else { return false }
        return at > bt
    }

    /// One lineage register to write: `successorHex` continues `rootHex`.
    struct Fold: Equatable {
        let successorHex: String
        let rootHex: String
    }

    /// The other person of a two-person roster, or `nil`.
    static func peerKey(people: [String], ownKeyB64: String) -> String? {
        guard people.count == 2 else { return nil }
        let peers = people.filter { $0 != ownKeyB64 }
        return peers.count == 1 ? peers[0] : nil
    }

    /// The registers that fold each peer-founded one-to-one into the
    /// conversation of an older one-to-one with the same person that this
    /// account founded. Only a group that is its own conversation (no lineage
    /// yet) is folded; when several own-founded one-to-ones with the person
    /// exist, the one whose conversation id sorts first is the root, so every
    /// device of the account writes the same register.
    static func folds(held: [Held], ownKeyB64: String?) -> [Fold] {
        guard let ownKeyB64 else { return [] }
        func peerOf(_ group: Held) -> String? {
            group.isOneToOne ? peerKey(people: group.people, ownKeyB64: ownKeyB64) : nil
        }
        var out: [Fold] = []
        for joined in held {
            guard let peer = peerOf(joined) else { continue }
            // Already part of a conversation (a successor, or a root others
            // continue).
            guard joined.conversationHex == joined.groupHex else { continue }
            if held.contains(where: { $0.groupHex != joined.groupHex && $0.conversationHex == joined.groupHex }) {
                continue
            }
            let roots = held
                .filter {
                    $0.groupHex != joined.groupHex
                        && $0.conversationHex != joined.conversationHex
                        && peerOf($0) == peer
                        // Newer, whoever founded either; else the peer's over
                        // this account's, unless this device knows the latter
                        // is the newer.
                        && (heldAfter(joined, $0)
                            || (joined.founderKeyB64 == peer && $0.founderKeyB64 == ownKeyB64
                                && !heldAfter($0, joined)))
                }
                .map(\.conversationHex)
                .sorted()
            if let root = roots.first { out.append(Fold(successorHex: joined.groupHex, rootHex: root)) }
        }
        return out
    }
}

/// When this device first held each conversation group, per account (base
/// scenario 9): a group joined later is recorded later, which orders two
/// one-to-ones with the same person for ``DuplicateOneToOne/folds(held:ownKeyB64:)``.
/// A group seen for the first time together with others (the first run after
/// an upgrade, a restore) shares their time and orders nothing. Device-local
/// (`UserDefaults`); never sent.
enum ConversationHeldSince {
    /// Pure: `stored` completed with `nowMs` for groups without a time, and
    /// groups no longer held dropped.
    static func next(stored: [String: Int64], groupHexes: [String], nowMs: Int64) -> [String: Int64] {
        var out: [String: Int64] = [:]
        for hex in groupHexes { out[hex] = stored[hex] ?? nowMs }
        return out
    }

    static var defaults: UserDefaults = .standard

    private static func key(_ accountIdB64: String) -> String {
        "lakko.conversationHeldSince." + accountIdB64
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
            .trimmingCharacters(in: CharacterSet(charactersIn: "="))
    }

    /// The record of `accountIdB64`, completed and stored.
    static func record(accountIdB64: String, groupHexes: [String], nowMs: Int64) -> [String: Int64] {
        let raw = defaults.dictionary(forKey: key(accountIdB64)) ?? [:]
        var stored: [String: Int64] = [:]
        for (hex, value) in raw {
            if let n = value as? NSNumber { stored[hex] = n.int64Value }
        }
        let next = next(stored: stored, groupHexes: groupHexes, nowMs: nowMs)
        if next != stored {
            defaults.set(next.mapValues { NSNumber(value: $0) }, forKey: key(accountIdB64))
        }
        return next
    }

    /// Forget device / delete account.
    static func clear(accountIdB64: String) {
        defaults.removeObject(forKey: key(accountIdB64))
    }
}
