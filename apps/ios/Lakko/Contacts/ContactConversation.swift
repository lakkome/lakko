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
