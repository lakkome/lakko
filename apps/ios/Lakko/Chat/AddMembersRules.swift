// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// One person picked on the Add members screen
/// (`docs/ux/conversation-info.md` section 2.4): what the conversation's add
/// path takes (the account id and the person's signature key), the name a
/// failure is reported under, and, for a person the search found who is not
/// a contact, what the result carried for reaching them (ADR-0060 section 6,
/// "An add of a person found by search").
public struct AddMembersPick: Equatable, Identifiable {
    /// The person's account (signature) key: what the add expects their
    /// devices to be bound to.
    public let personKey: Data
    public let accountId: Data
    /// The name shown on the chip and in "Could not add {names}".
    public let name: String
    /// The reach grant the search read for the person and this device does
    /// not hold yet (an introduction's `fof` grant): kept before the add, so
    /// the committer delegations are signed with it.
    public let foundGrant: FfiReachGrant?
    /// The verified mailbox of a person found in Everyone (their public
    /// record's binding): the add posts a committer-delegation envelope with
    /// no delegation naming it, so a committer may write the Welcome there
    /// without a proof.
    public let openMailbox: MailboxTarget?

    public var id: Data { personKey }

    public init(
        personKey: Data, accountId: Data, name: String,
        foundGrant: FfiReachGrant? = nil, openMailbox: MailboxTarget? = nil
    ) {
        self.personKey = personKey
        self.accountId = accountId
        self.name = name
        self.foundGrant = foundGrant
        self.openMailbox = openMailbox
    }
}

/// Who the Add members screen lists and who can be picked
/// (`docs/ux/conversation-info.md` section 2.4). Pure, so the rules run in a
/// test; the screen only renders what these return.
public enum AddMembersRules {

    /// One accepted contact, as the screen reads it.
    public struct Contact: Equatable {
        public let personKey: Data
        /// `nil` for a contact whose request carried no account id
        /// (`new-group.md` section 3): listed, dimmed, never pickable.
        public let accountId: Data?
        public let name: String
        public let username: String?

        public init(personKey: Data, accountId: Data?, name: String, username: String?) {
            self.personKey = personKey
            self.accountId = accountId
            self.name = name
            self.username = username
        }
    }

    /// One row of the contacts list.
    public struct ContactRow: Equatable, Identifiable {
        public let contact: Contact
        public var pickable: Bool { contact.accountId != nil }
        public var id: Data { contact.personKey }
    }

    /// The contacts the list shows for `query`: never one already in the
    /// group, nor one with an add waiting for the sweep. Keys are compared
    /// after `current` (a proven predecessor key names the same person).
    public static func contactRows(
        contacts: [Contact],
        memberKeys: Set<Data>,
        pendingKeys: Set<Data>,
        query: String,
        current: (Data) -> Data = { $0 }
    ) -> [ContactRow] {
        let members = Set(memberKeys.map(current))
        let pending = Set(pendingKeys.map(current))
        var seen = Set<Data>()
        return contacts.compactMap { contact in
            let key = current(contact.personKey)
            guard !members.contains(key), !pending.contains(key), seen.insert(key).inserted,
                  matches(query: query, name: contact.name, username: contact.username)
            else { return nil }
            return ContactRow(contact: contact)
        }
    }

    /// Loose local matching from the first letter: case- and accent-
    /// insensitive, on the name or the username (a leading `@` is ignored).
    /// An empty query matches everyone.
    public static func matches(query: String, name: String, username: String?) -> Bool {
        var typed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        if typed.hasPrefix("@") { typed.removeFirst() }
        guard !typed.isEmpty else { return true }
        let needle = fold(typed)
        if fold(name).contains(needle) { return true }
        if let username, fold(username).contains(needle) { return true }
        return false
    }

    private static func fold(_ text: String) -> String {
        text.folding(options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive], locale: nil)
    }

    /// The network results shown under People: never a member, a person with
    /// an add waiting, this account, or a contact (contacts are the list
    /// above, so a person is never shown twice).
    public static func networkRows(
        found: [FoundPerson],
        contactKeys: Set<Data>,
        memberKeys: Set<Data>,
        pendingKeys: Set<Data>,
        ownAccountKey: Data?,
        current: (Data) -> Data = { $0 }
    ) -> [FoundPerson] {
        var excluded = Set((contactKeys.union(memberKeys).union(pendingKeys)).map(current))
        if let ownAccountKey { excluded.insert(current(ownAccountKey)) }
        var seen = Set<Data>()
        return found.filter { person in
            let key = current(person.accountKey)
            return person.reach != .contact && !excluded.contains(key) && seen.insert(key).inserted
        }
    }

    /// Whether a network result can be picked: the add needs the person's
    /// account id, and finding them must have given a way to reach them (a
    /// person shown by name only, who does not let this account find them,
    /// is not).
    public static func isPickable(_ person: FoundPerson) -> Bool {
        person.accountId != nil && person.reach != .none
    }

    /// The pick a network result becomes, `nil` when it is not pickable. It
    /// keeps what the result carries for reaching the person: the grant the
    /// search read (a person found through a grant they handed out), or the
    /// verified mailbox of a person found in Everyone. A mailbox that is not
    /// the person's (its binding verified under another key) is dropped.
    public static func pick(_ person: FoundPerson, fallbackName: String) -> AddMembersPick? {
        guard isPickable(person), let accountId = person.accountId else { return nil }
        let name = person.displayName ?? person.username.map { "@\($0)" } ?? fallbackName
        let open = person.reach == .open ? person.mailbox.flatMap {
            $0.accountKey == person.accountKey ? $0 : nil
        } : nil
        return AddMembersPick(
            personKey: person.accountKey, accountId: accountId, name: name,
            foundGrant: person.reach == .grant ? person.grant : nil,
            openMailbox: open)
    }

    /// The pick a contact row becomes, `nil` when it is not pickable.
    public static func pick(_ row: ContactRow) -> AddMembersPick? {
        guard let accountId = row.contact.accountId else { return nil }
        return AddMembersPick(personKey: row.contact.personKey, accountId: accountId, name: row.contact.name)
    }

    /// The trailing button's catalog key and arguments: "Add" for none or
    /// one pick, "Add {count}" from two.
    public static func submitLabel(count: Int) -> (key: String, count: Int?) {
        count >= 2 ? ("chat.members.add.submit_count", count) : ("chat.members.add.submit", nil)
    }

    /// Toggle `pick` in `picks`, keeping the order picks were made in.
    public static func toggling(_ pick: AddMembersPick, in picks: [AddMembersPick]) -> [AddMembersPick] {
        if picks.contains(where: { $0.personKey == pick.personKey }) {
            return picks.filter { $0.personKey != pick.personKey }
        }
        return picks + [pick]
    }

    /// The people with an add waiting for the sweep: this device's own add
    /// requests whose device proposals are still pending, plus any pending add
    /// whose leaf key is the person's own key.
    public static func pendingAddPersonKeys(
        pendingAddLeafKeysB64: Set<String>,
        intents: [MembershipIntent]
    ) -> Set<Data> {
        var out = Set(pendingAddLeafKeysB64.compactMap { Data(base64Encoded: $0) })
        for intent in intents {
            guard case .add(let personKeyB64, _, let leafKeysB64) = intent.request,
                  leafKeysB64.contains(where: pendingAddLeafKeysB64.contains) || pendingAddLeafKeysB64.contains(personKeyB64),
                  let key = Data(base64Encoded: personKeyB64)
            else { continue }
            out.insert(key)
        }
        return out
    }
}

/// The Add button's run (`docs/ux/conversation-info.md` section 2.4): each
/// pick through the conversation's add path, one after another, then one
/// sweep. A pick that fails does not stop the others. Returns the picks that
/// could not be added, in pick order.
public enum AddMembersRun {
    @MainActor
    public static func run(
        picks: [AddMembersPick],
        add: (AddMembersPick) async throws -> Void,
        sweep: () async -> Void
    ) async -> [AddMembersPick] {
        var failed: [AddMembersPick] = []
        for pick in picks {
            do {
                try await add(pick)
            } catch {
                NSLog("Add members: an add failed: %@", String(describing: type(of: error)))
                failed.append(pick)
            }
        }
        if failed.count < picks.count {
            await sweep()
        }
        return failed
    }
}
