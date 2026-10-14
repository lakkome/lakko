// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The list `Invite contacts` shows on the call screen, and what choosing a
/// row does (`docs/ux/call-links.md` section 3).
///
/// Mirror of `apps/web/src/calls/invite-contacts.ts` and of
/// `sendLinkCallInvite` in `apps/web/src/calls/CallHostContext.tsx`.
///
/// The person's CONTACTS, not their conversations: an invitation is addressed
/// to a person, and a contact the person has never written to is as invitable
/// as one they talk to daily. (The first version listed conversations, so a
/// person with contacts and no chats saw no way to invite anyone, and groups
/// were offered as invitees; corrected 2026-10-02.) Groups are not in it.

/// One row of the list.
public struct InviteContactRow: Equatable, Identifiable {
    /// The contact's person (account) key: who the invitation goes to.
    public let key: Data
    public let name: String
    /// The contact's username without `@`, when one is known. What a typed
    /// query is matched against beside the name.
    public let username: String?
    /// `true` when `name` is the neutral placeholder (nothing is known about
    /// the person).
    public let isPlaceholder: Bool
    /// `true` when `name` already is the username: the row then shows no
    /// second `@username` line under it.
    public let nameIsUsername: Bool

    public init(
        key: Data, name: String, username: String?,
        isPlaceholder: Bool = false, nameIsUsername: Bool = false
    ) {
        self.key = key
        self.name = name
        self.username = username
        self.isPlaceholder = isPlaceholder
        self.nameIsUsername = nameIsUsername
    }

    /// The key, base64: the row's identity, and the id an invitation is
    /// recorded under (``CallInviteOffer/invited``).
    public var id: String { key.base64EncodedString() }

    /// `@username` under the name, when one is known and the name is not it.
    public var usernameLine: String? {
        guard let username, !nameIsUsername else { return nil }
        return "@\(username)"
    }
}

enum InviteContacts {

    /// The rows, by name, the way the Contacts list orders people
    /// (``ContactListPresentation/sections(_:)``, flattened). A contact whose
    /// record is doubled is listed once.
    static func rows<Contact>(
        _ contacts: [Contact],
        row: (Contact) -> InviteContactRow
    ) -> [InviteContactRow] {
        var byId: [String: InviteContactRow] = [:]
        var items: [ContactListPresentation.Item<String>] = []
        for contact in contacts {
            let made = row(contact)
            guard byId[made.id] == nil else { continue }
            byId[made.id] = made
            items.append(item(made))
        }
        return ContactListPresentation.sections(items)
            .flatMap(\.ids)
            .compactMap { byId[$0] }
    }

    /// The rows a typed query keeps: by name or by username, the Contacts
    /// list's own match (``ContactListPresentation/matches(_:query:)``).
    static func filter(_ rows: [InviteContactRow], query: String) -> [InviteContactRow] {
        rows.filter { ContactListPresentation.matches(item($0), query: query) }
    }

    private static func item(_ row: InviteContactRow) -> ContactListPresentation.Item<String> {
        ContactListPresentation.Item(
            id: row.id, name: row.name, username: row.username, isPlaceholder: row.isPlaceholder)
    }
}

// MARK: - Sending the invitation

/// Why an invitation could not be addressed.
enum InviteContactSendError: Error, Equatable {
    /// The chosen key is not a contact of this account any more.
    case unknownContact
    /// There is no conversation with the contact on this device, and the
    /// contact record holds no account id to found one with.
    case contactHasNoAccountId
    /// The chat layer is not there to found a conversation in.
    case chatUnavailable
}

/// Send a call's invitation to a contact: into the one-to-one conversation
/// with them, founding it first when this device holds none.
///
/// Written against ports so the order is what a test drives.
@MainActor
enum InviteContactSend {

    struct Ports {
        /// The accepted contact with this person key, or `nil`.
        var contact: @MainActor (_ key: Data) -> AcceptedContact?
        /// The one-to-one with the contact on this device
        /// (``ContactConversation``), or `nil` when there is none.
        var conversationWith: @MainActor (_ contact: AcceptedContact) -> String?
        /// Found the one-to-one with the contact and link it on the contact
        /// record: the one creation path the contact flows share
        /// (``ContactsRepository/foundOneToOne(with:)``).
        var found: @MainActor (_ contact: AcceptedContact) async throws -> String
        /// Send a plain text message into a conversation through the normal
        /// send path, which also records the sent row there.
        var send: @MainActor (_ groupIdHex: String, _ text: String) async throws -> Void
    }

    /// The existing conversation is used. With none, one is founded, once,
    /// and only for a contact whose record holds an account id (every contact
    /// accepted since ADR-0021 does); then the message goes out. Any failure
    /// throws and the caller reports the invitation as not sent.
    static func run(contactKey: Data, text: String, ports: Ports) async throws {
        guard let contact = ports.contact(contactKey) else {
            throw InviteContactSendError.unknownContact
        }
        let conversation: String
        if let existing = ports.conversationWith(contact) {
            conversation = existing
        } else {
            guard contact.request.fromAccountId?.count == 16 else {
                throw InviteContactSendError.contactHasNoAccountId
            }
            conversation = try await ports.found(contact)
        }
        try await ports.send(conversation, text)
    }
}
