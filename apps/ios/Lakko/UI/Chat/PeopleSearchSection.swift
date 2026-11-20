// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// `docs/ux/who-can-find-me.md` section 6.1, "One list" (2026-11-04): what
/// one search field adds to the screen's own list, in the same list and with
/// no heading: the people this device knows, then what the network found
/// (people, public groups and channels), each row saying what it is on its
/// second line. The spinner, a failure and "Nothing found" stand at the end of
/// the list; "Nothing found" only when the whole list is empty. Nothing to
/// press: the field asks the network by itself (``PeopleSearchModel``).
///
/// The same view under the Chats field (`includesChannels`, which also shows
/// groups and channels found by `@name` or name) and under the Contacts field
/// (people only, and no local contact rows: the Contacts list shows them).
struct PeopleSearchSection: View {
    @ObservedObject var model: PeopleSearchModel
    @ObservedObject var session: PeopleSearchSession
    let includesChannels: Bool
    let showsLocalContacts: Bool
    /// Whether the screen's own list shows rows above this view (matching
    /// conversations, contacts): then "Nothing found" never shows.
    let listShowsRows: Bool
    /// The people a list row above shows (a contact, someone added through
    /// their invite link): one person, one row.
    let listed: Set<Data>
    /// The people whose one-to-one is among the matching conversations above.
    let chatPeers: Set<Data>
    /// The conversations the list above already shows: not repeated.
    let listedConversations: Set<String>
    let onOpenPerson: (FoundPerson) -> Void
    let onOpenChannel: (_ channelIdHex: String, _ handle: String) -> Void
    /// The name of a channel this device holds, `nil` when it knows none.
    let channelName: (_ channelIdHex: String) -> String?
    /// ADR-0087 section 5 — a public group or channel row was opened (its card).
    var onOpenConversation: (ConversationHandles.Found) -> Void = { _ in }
    /// Whether this device holds a found group or channel: its row reads
    /// `Open` and opens it, with no card.
    let isHeld: (String) -> Bool
    let onOpenHeld: (String) -> Void
    @ObservedObject private var l10n = Localization.shared
    /// ADR-0099 step 4 — a person found suspended leaves the list as the
    /// answer arrives.
    @ObservedObject private var suspensions = SuspensionMarks.shared

    init(
        model: PeopleSearchModel,
        includesChannels: Bool,
        showsLocalContacts: Bool,
        listShowsRows: Bool = false,
        listed: Set<Data> = [],
        chatPeers: Set<Data> = [],
        listedConversations: Set<String> = [],
        onOpenPerson: @escaping (FoundPerson) -> Void,
        onOpenChannel: @escaping (String, String) -> Void = { _, _ in },
        channelName: @escaping (String) -> String? = { _ in nil },
        onOpenConversation: @escaping (ConversationHandles.Found) -> Void = { _ in },
        isHeld: @escaping (String) -> Bool = { _ in false },
        onOpenHeld: @escaping (String) -> Void = { _ in }
    ) {
        self.onOpenConversation = onOpenConversation
        self.channelName = channelName
        self.model = model
        self.session = model.session
        self.includesChannels = includesChannels
        self.showsLocalContacts = showsLocalContacts
        self.listShowsRows = listShowsRows
        self.listed = listed
        self.chatPeers = chatPeers
        self.listedConversations = listedConversations
        self.onOpenPerson = onOpenPerson
        self.onOpenChannel = onOpenChannel
        self.isHeld = isHeld
        self.onOpenHeld = onOpenHeld
    }

    var body: some View {
        let rows = model.rows(showsLocalContacts: showsLocalContacts, listed: listed, chatPeers: chatPeers)
        let conversations = includesChannels
            ? (model.answer?.conversations ?? []).filter { !listedConversations.contains($0.idHex) } : []
        let channel = includesChannels ? model.answer?.channel.flatMap { found in
            listedConversations.contains(found.idHex) || conversations.contains { $0.idHex == found.idHex }
                ? nil : found
        } : nil
        let anyRow = listShowsRows || !rows.local.isEmpty || !rows.network.isEmpty
            || !conversations.isEmpty || channel != nil
        let message = model.peopleMessage(
            includesChannels: includesChannels, showsLocalContacts: showsLocalContacts,
            listed: listed, chatPeers: chatPeers, listShowsRows: anyRow)
        VStack(alignment: .leading, spacing: 10) {
            ForEach(rows.local) { person in
                personRow(person)
            }
            ForEach(rows.network) { person in
                personRow(person)
            }
            ForEach(conversations) { found in
                let held = isHeld(found.idHex)
                Button { held ? onOpenHeld(found.idHex) : onOpenConversation(found) } label: {
                    ConversationFoundRow(found: found, opens: held)
                }
                .buttonStyle(.plain)
            }
            if let channel {
                Button { onOpenChannel(channel.idHex, channel.handle) } label: {
                    HStack(spacing: 12) {
                        LkIconView(.megaphone, size: 20)
                            .frame(width: 40, height: 40)
                            .foregroundStyle(Color.lakkoAccent)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(Self.channelRowTitle(channel, name: channelName(channel.idHex)))
                                .foregroundStyle(Color.lakkoText)
                            Text(l10n.t("nav.channels"))
                                .font(.footnote)
                                .foregroundStyle(Color.lakkoTextMuted)
                        }
                        Spacer()
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
            // The state line, at the end of the list.
            if model.searching {
                HStack {
                    Spacer()
                    ProgressView().controlSize(.small)
                    Spacer()
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(l10n.t("search.people.searching_a11y"))
            } else if let message {
                Text(message)
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .frame(maxWidth: .infinity, alignment: anyRow ? .leading : .center)
                    .multilineTextAlignment(anyRow ? .leading : .center)
                    .accessibilityAddTraits(.updatesFrequently)
            } else if !anyRow, !model.asksNetwork {
                Text(l10n.t("search.nothing_found"))
                    .foregroundStyle(Color.lakkoTextMuted)
                    .frame(maxWidth: .infinity)
                    .multilineTextAlignment(.center)
            }
        }
        // ADR-0099 step 4 — ask about every person found; the rows are shown
        // at once and a suspended one drops out when its standing arrives.
        .task(id: model.suspensionCheckKeys) {
            suspensions.check(model.suspensionCheckKeys)
        }
    }

    /// What a found channel's row reads: its name when one is known, else its
    /// `@handle`.
    static func channelRowTitle(_ channel: FoundChannel, name: String?) -> String {
        let known = name?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return known.isEmpty ? "@\(channel.handle)" : known
    }

    private func personRow(_ person: FoundPerson) -> some View {
        FoundPersonRow(
            person: person,
            state: session.requestState(for: person),
            onAdd: { Task { await session.addContact(person) } },
            onAskAgain: session.canAskAgain(person)
                ? { Task { await session.askAgain(person) } } : nil,
            onOpen: { onOpenPerson(person) })
    }
}

/// One found person: avatar, name, `@username`, and Open (a contact), Add
/// contact, or the request's outcome.
private struct FoundPersonRow: View {
    let person: FoundPerson
    let state: PersonRequestState
    let onAdd: () -> Void
    /// **Ask again** under "Request sent", `nil` when there is nothing to
    /// repeat.
    let onAskAgain: (() -> Void)?
    let onOpen: () -> Void
    @ObservedObject private var l10n = Localization.shared
    /// ADR-0099 step 3 — a suspended account adds nobody.
    @Environment(\.accountSuspended) private var accountSuspended

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
            VStack(alignment: .trailing, spacing: 2) {
                Text(l10n.t("search.people.request_sent"))
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
                if let onAskAgain, !accountSuspended {
                    Button(l10n.t("contacts.search.ask_again"), action: onAskAgain)
                        .buttonStyle(.borderless)
                }
            }
        } else if person.canAddContact, !accountSuspended {
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


/// ADR-0087 section 5 — one public group or channel: avatar, name, `@name`,
/// and its kind and size band.
struct ConversationFoundRow: View {
    let found: ConversationHandles.Found
    /// This device is in it: the row reads `Open` and opens it.
    var opens: Bool = false
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        HStack(spacing: 12) {
            InitialsAvatar(
                name: found.name, identity: found.idHex, size: 40, isGroup: true,
                photo: found.avatar.map(AvatarPhoto.avatar))
            VStack(alignment: .leading, spacing: 2) {
                Text(found.name).foregroundStyle(Color.lakkoText).lineLimit(1)
                Text("@" + found.handle)
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .environment(\.layoutDirection, .leftToRight)
                Text(Self.line(found, l10n: l10n))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            Spacer()
            if opens {
                Text(l10n.t("search.conversation.open"))
                    .foregroundStyle(Color.lakkoAccent)
            }
        }
        .contentShape(Rectangle())
    }

    /// "Public group · 100+ members" / "Public channel · 10k+ subscribers".
    static func line(_ found: ConversationHandles.Found, l10n: Localization) -> String {
        let band = ["<10", "10+", "100+", "1k+", "10k+", "100k+"][Int(min(found.sizeBand, 5))]
        return l10n.t(
            found.kind == .channel ? "search.conversation.channel_line" : "search.conversation.group_line",
            texts: ["band": band])
    }
}
