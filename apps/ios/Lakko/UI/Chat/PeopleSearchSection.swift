// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// `docs/ux/who-can-find-me.md` section 6.1 — what one search field shows
/// beside its own list: under the heading **People**, the people this device
/// knows, then what the network found. Nothing to press: the field asks the
/// network by itself (``PeopleSearchModel``). A row is an avatar, the name,
/// the `@username` when verified and **Add contact**, which sends the request
/// at once. A row never says why the person was found.
///
/// The same view under the Chats field (`includesChannels`, which also shows
/// a channel found by its handle) and under the Contacts field (people only,
/// and no local contact rows: the Contacts list already shows them).
struct PeopleSearchSection: View {
    @ObservedObject var model: PeopleSearchModel
    @ObservedObject var session: PeopleSearchSession
    let includesChannels: Bool
    let showsLocalContacts: Bool
    /// The line shown when the field shows nothing of its own and does not
    /// ask the network, whose People part answers by itself (Chats: "No
    /// results" when no conversation matched either); `nil` shows nothing.
    let emptyText: String?
    /// The people whose one-to-one is among the matching conversations above.
    let chatPeers: Set<Data>
    let onOpenPerson: (FoundPerson) -> Void
    let onOpenChannel: (_ channelIdHex: String, _ handle: String) -> Void
    @ObservedObject private var l10n = Localization.shared

    init(
        model: PeopleSearchModel,
        includesChannels: Bool,
        showsLocalContacts: Bool,
        emptyText: String? = nil,
        chatPeers: Set<Data> = [],
        onOpenPerson: @escaping (FoundPerson) -> Void,
        onOpenChannel: @escaping (String, String) -> Void = { _, _ in }
    ) {
        self.model = model
        self.session = model.session
        self.includesChannels = includesChannels
        self.showsLocalContacts = showsLocalContacts
        self.emptyText = emptyText
        self.chatPeers = chatPeers
        self.onOpenPerson = onOpenPerson
        self.onOpenChannel = onOpenChannel
    }

    var body: some View {
        let rows = model.rows(showsLocalContacts: showsLocalContacts, chatPeers: chatPeers)
        let channel = includesChannels ? model.answer?.channel : nil
        let message = model.peopleMessage(
            includesChannels: includesChannels, showsLocalContacts: showsLocalContacts,
            chatPeers: chatPeers)
        // No heading without something under it. Below 3 characters nothing
        // was asked, so a person row is all that can stand here.
        let showsHeading = model.searching || !rows.local.isEmpty || !rows.network.isEmpty
            || channel != nil || message != nil
        VStack(alignment: .leading, spacing: 10) {
            if let emptyText, !model.asksNetwork,
               !model.showsAnything(includesChannels: includesChannels, showsLocalContacts: showsLocalContacts) {
                Text(emptyText)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .frame(maxWidth: .infinity)
            }
            if showsHeading {
                HStack(spacing: 8) {
                    Text(l10n.t("search.people.heading"))
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Color.lakkoTextMuted)
                    if model.searching {
                        ProgressView().controlSize(.small)
                            .accessibilityLabel(l10n.t("search.people.searching_a11y"))
                    }
                }
                .accessibilityElement(children: .combine)
                .accessibilityAddTraits(.isHeader)
                ForEach(rows.local) { person in
                    personRow(person)
                }
                if let channel {
                    Button { onOpenChannel(channel.idHex, channel.handle) } label: {
                        HStack(spacing: 12) {
                            Image(systemName: "megaphone")
                                .frame(width: 40, height: 40)
                                .foregroundStyle(Color.lakkoAccent)
                            Text(l10n.t("search.people.channel_result", texts: ["handle": "@\(channel.handle)"]))
                                .foregroundStyle(Color.lakkoText)
                            Spacer()
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
                ForEach(rows.network) { person in
                    personRow(person)
                }
                if let message {
                    Text(message)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .accessibilityAddTraits(.updatesFrequently)
                }
            }
        }
    }

    private func personRow(_ person: FoundPerson) -> some View {
        FoundPersonRow(
            person: person,
            state: session.requestState(for: person),
            onAdd: { Task { await session.addContact(person) } },
            onOpen: { onOpenPerson(person) })
    }
}

/// One found person: avatar, name, `@username`, and Open (a contact), Add
/// contact, or the request's outcome.
private struct FoundPersonRow: View {
    let person: FoundPerson
    let state: PersonRequestState
    let onAdd: () -> Void
    let onOpen: () -> Void
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        HStack(spacing: 12) {
            InitialsAvatar(
                name: person.displayName ?? person.username ?? "?",
                identity: person.accountKey.base64EncodedString(),
                size: 40,
                photo: .person(person.accountKey))
            VStack(alignment: .leading, spacing: 2) {
                Text(person.displayName ?? l10n.t("contacts.request.someone"))
                    .foregroundStyle(Color.lakkoText)
                if let username = person.username {
                    Text("@\(username)")
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
                if let message = state.message(canAddContact: person.canAddContact) {
                    Text(message)
                        .font(.footnote)
                        .foregroundStyle(state == .refused ? Color.lakkoTextMuted : Color.lakkoDanger)
                        .accessibilityAddTraits(.updatesFrequently)
                }
            }
            Spacer()
            trailing
        }
    }

    /// A contact opens the conversation; someone who does not let this
    /// account find them is shown by name only (web `canAddContact`).
    @ViewBuilder
    private var trailing: some View {
        if person.reach == .contact {
            Button(l10n.t("common.open"), action: onOpen)
                .buttonStyle(.borderless)
        } else if state == .sent {
            // From the outgoing list, whatever the check says of them.
            Text(l10n.t("search.people.request_sent"))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
        } else if person.canAddContact {
            switch state {
            case .idle, .failed:
                Button(l10n.t("search.people.add_contact"), action: onAdd)
                    .buttonStyle(.borderless)
            case .sending:
                ProgressView().controlSize(.small)
            case .sent, .refused:
                EmptyView()
            }
        }
    }
}
