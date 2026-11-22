// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// Group info > Add members (`docs/ux/conversation-info.md` section 2.4):
/// pushed over the group's info, with the system back returning to it.
///
/// A search field under the bar, the picked people as removable chips, the
/// contacts with a checkbox (filtered on the device from the first letter),
/// and, from three characters, the people the network search finds under
/// **People** (`docs/ux/who-can-find-me.md` section 6.1, the same search the
/// Chats and Contacts fields run, on their shared session). **Add** returns to
/// the info at once and adds each pick through the conversation's add path
/// (``ChatViewModel/onAddMembers(_:)``). Who is listed and who can be picked
/// is ``AddMembersRules``.
struct AddMembersScreen: View {
    @ObservedObject var viewModel: ChatViewModel
    @ObservedObject var contacts: ContactsViewModel
    /// Back to the info, at once, when Add is pressed.
    let onAdded: () -> Void

    @StateObject private var search: PeopleSearchModel
    @ObservedObject private var directory = ProfileDirectory.shared
    @ObservedObject private var l10n = Localization.shared
    @State private var query = ""
    @State private var picks: [AddMembersPick] = []

    init(
        viewModel: ChatViewModel,
        contacts: ContactsViewModel,
        session: PeopleSearchSession,
        onAdded: @escaping () -> Void
    ) {
        self.viewModel = viewModel
        self.contacts = contacts
        self.onAdded = onAdded
        _search = StateObject(wrappedValue: PeopleSearchModel(session: session))
    }

    private static func current(_ key: Data) -> Data {
        ReachCenter.shared.currentAccountKey(of: key)
    }

    /// Every accepted contact: the name, and the username as the second line.
    private var allContacts: [AddMembersRules.Contact] {
        let _ = directory.revision
        return contacts.repositoryState.accepted.map { entry in
            AddMembersRules.contact(
                personKey: entry.request.fromPubkey,
                accountId: entry.request.fromAccountId,
                resolved: directory.resolve(signatureKey: entry.request.fromPubkey))
        }
    }

    var body: some View {
        let everyone = allContacts
        let members = viewModel.memberPersonKeys
        let pending = viewModel.pendingAddPersonKeys
        let contactRows = AddMembersRules.contactRows(
            contacts: everyone, memberKeys: members, pendingKeys: pending,
            query: query, current: Self.current)
        let networkRows = AddMembersRules.networkRows(
            found: search.answer?.people ?? [],
            contactKeys: Set(everyone.map(\.personKey)),
            memberKeys: members, pendingKeys: pending,
            ownAccountKey: ReachCenter.shared.ownAccountKey,
            current: Self.current)
        let state = search.networkState(includesChannels: false)
        let message = (state == .found) ? nil : state?.message
        let showsPeople = search.asksNetwork
            && (search.searching || !networkRows.isEmpty || message != nil)
        List {
            Section {
                HStack(spacing: 8) {
                    LkIconView(.search, size: 20)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .accessibilityHidden(true)
                    TextField(l10n.t("chat.members.add.search_placeholder"), text: $query)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled(true)
                        .accessibilityIdentifier("add-members-search")
                }
                if !picks.isEmpty {
                    chips
                }
            }
            Section {
                ForEach(contactRows) { row in
                    contactRow(row)
                }
            }
            if showsPeople {
                Section {
                    ForEach(networkRows) { person in
                        networkRow(person)
                    }
                    if let message {
                        Text(message)
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                            .accessibilityAddTraits(.updatesFrequently)
                    }
                } header: {
                    HStack(spacing: 8) {
                        Text(l10n.t("search.people.heading"))
                        if search.searching {
                            ProgressView().controlSize(.small)
                                .accessibilityLabel(l10n.t("search.people.searching_a11y"))
                        }
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityAddTraits(.isHeader)
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(l10n.t("chat.members.add.title"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                let label = AddMembersRules.submitLabel(count: picks.count)
                Button(
                    label.count.map {
                        l10n.t(label.key, texts: ["count": $0.formatted(.number.locale(Localization.currentLocale))])
                    } ?? l10n.t(label.key)
                ) {
                    let chosen = picks
                    // Section 2.4 — back to the info at once; the adds run
                    // there, and what could not be added is reported there.
                    viewModel.onAddMembers(chosen)
                    onAdded()
                }
                .disabled(picks.isEmpty || viewModel.membershipBusy)
                .accessibilityIdentifier("add-members-submit")
            }
        }
        .onChange(of: query) { _, value in search.update(query: value) }
        .onAppear { viewModel.refreshMembers() }
    }

    /// The picked people, each a chip that un-picks it.
    private var chips: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(picks) { pick in
                    Button {
                        picks = picks.filter { $0.personKey != pick.personKey }
                    } label: {
                        HStack(spacing: 4) {
                            Text(pick.name)
                                .lineLimit(1)
                            LkIconView(.xmark, size: 14, relativeTo: .caption2)
                        }
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoText)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 6)
                        .background(Capsule().fill(Color.lakkoAccentSoft))
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(pick.name)
                    .accessibilityHint(l10n.t("common.remove"))
                }
            }
        }
    }

    private func isPicked(_ key: Data) -> Bool {
        picks.contains { $0.personKey == key }
    }

    @ViewBuilder
    private func contactRow(_ row: AddMembersRules.ContactRow) -> some View {
        let contact = row.contact
        let isOn = row.pickable && isPicked(contact.personKey)
        Button {
            guard let pick = AddMembersRules.pick(row) else { return }
            picks = AddMembersRules.toggling(pick, in: picks)
        } label: {
            personLine(
                name: contact.name, username: contact.username, key: contact.personKey,
                note: row.pickable ? nil : l10n.t("chats.new_group.accept_again"),
                pickable: row.pickable, isOn: isOn)
        }
        .disabled(!row.pickable)
        .accessibilityAddTraits(isOn ? .isSelected : [])
    }

    @ViewBuilder
    private func networkRow(_ person: FoundPerson) -> some View {
        let pickable = AddMembersRules.isPickable(person)
        let isOn = pickable && isPicked(person.accountKey)
        Button {
            guard let pick = AddMembersRules.pick(
                person, fallbackName: l10n.t("contacts.request.someone")) else { return }
            picks = AddMembersRules.toggling(pick, in: picks)
        } label: {
            personLine(
                name: person.displayName ?? l10n.t("contacts.request.someone"),
                username: person.username, key: person.accountKey,
                note: nil, pickable: pickable, isOn: isOn)
        }
        .disabled(!pickable)
        .accessibilityAddTraits(isOn ? .isSelected : [])
    }

    private func personLine(
        name: String, username: String?, key: Data, note: String?, pickable: Bool, isOn: Bool
    ) -> some View {
        HStack(spacing: 12) {
            InitialsAvatar(
                name: name,
                identity: key.base64EncodedString(),
                size: 40,
                photo: .person(key))
            VStack(alignment: .leading, spacing: 2) {
                Text(name)
                    .foregroundStyle(pickable ? Color.lakkoText : Color.lakkoTextMuted)
                if let username {
                    Text("@\(username)")
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
                if let note {
                    Text(note)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
            }
            Spacer()
            LkIconView(isOn ? .checkmarkCircleFill : .circle, size: 20)
                .foregroundStyle(isOn ? Color.lakkoAccent : Color.lakkoTextMuted)
                .opacity(pickable ? 1 : 0.4)
                .accessibilityHidden(true)
        }
        .contentShape(Rectangle())
    }
}
