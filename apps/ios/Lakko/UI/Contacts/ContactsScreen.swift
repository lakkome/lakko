// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The Contacts tab (docs/ux/client-redesign.md section 5.3): a searchable
/// list with the incoming requests on top (Accept / Decline on the row), the
/// sent requests under a collapsed "Sent" header, then the contacts in
/// alphabetical sections with an index on the right. The search field is the
/// one field of `docs/ux/who-can-find-me.md` section 6.1: it filters the
/// contacts and, from three characters, finds people on the network under
/// People, with Add contact on each row. The add menu holds "Share invite
/// link"; a row pushes ``ContactDetailScreen``.
///
/// Mirror of `apps/web/src/contacts/ContactsRoute.tsx` and
/// `apps/android/.../ui/contacts/ContactsScreen.kt`.
///
/// Receiving is automatic: the `.task` long-poll loop runs while the screen is
/// on-screen, and pull-to-refresh runs the same poll once more on demand.
/// Every repository call the previous screen made is still made from here or
/// from the sheets it presents.
public struct ContactsScreen: View {

    @ObservedObject public var viewModel: ContactsViewModel
    /// ADR-0060 — the people half of the search field (people only: no
    /// channel row). `nil` filters the contact list only.
    let peopleSearch: PeopleSearchModel?
    /// **Open** on a found person who is already a contact.
    let onOpenPerson: (FoundPerson) -> Void
    /// A scanned Lakko link (`/i/`, `/g/`, `/c/`), handed to the same route
    /// an opened Universal Link takes (``InviteIntake/handle(_:)``).
    let onOpenLink: (URL) -> Void

    /// ADR-0048 — names resolve through the directory; observing it here
    /// regroups the list in place when a profile or a verdict arrives.
    @ObservedObject private var profiles = ProfileDirectory.shared
    /// ADR-0061 — the title follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    @State private var query = ""
    @State private var inviteLinksPresented = false
    @State private var scanPresented = false
    @State private var addMenuOpen = false
    @State private var sentExpanded = false

    public init(
        viewModel: ContactsViewModel,
        peopleSearch: PeopleSearchModel? = nil,
        onOpenPerson: @escaping (FoundPerson) -> Void = { _ in },
        onOpenLink: @escaping (URL) -> Void = { _ in }
    ) {
        self.viewModel = viewModel
        self.peopleSearch = peopleSearch
        self.onOpenPerson = onOpenPerson
        self.onOpenLink = onOpenLink
    }

    // MARK: - Derived

    private var isSearching: Bool {
        !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private var contactsById: [Int64: AcceptedContact] {
        Dictionary(
            viewModel.repositoryState.accepted.map { ($0.request.messageId, $0) },
            uniquingKeysWith: { first, _ in first })
    }

    private var invitedById: [String: PendingContact] {
        Dictionary(viewModel.shownInvited.map { ($0.accountIdHex, $0) }, uniquingKeysWith: { first, _ in first })
    }

    /// The contacts as the grouping sees them, already filtered by the search.
    /// ADR-0079 — a person added through their invite link is listed at once,
    /// under the name the link states, mixed in alphabetically.
    private var items: [ContactListPresentation.Item<ContactRowId>] {
        // Reading `revision` subscribes this body to every profile change.
        _ = profiles.revision
        let accepted = viewModel.repositoryState.accepted.compactMap { contact -> ContactListPresentation.Item<ContactRowId>? in
            let resolved = profiles.resolve(signatureKey: contact.request.fromPubkey)
            let item = ContactListPresentation.Item(
                id: ContactRowId.contact(contact.request.messageId),
                name: resolved.name,
                username: resolved.verifiedUsername,
                isPlaceholder: resolved.isPlaceholder)
            return ContactListPresentation.matches(item, query: query) ? item : nil
        }
        let invited = viewModel.shownInvited.compactMap { contact -> ContactListPresentation.Item<ContactRowId>? in
            let stated = PendingContacts.name(contact)
            let item = ContactListPresentation.Item(
                id: ContactRowId.invited(contact.accountIdHex),
                name: stated ?? l10n.t("invite.contact.page.unnamed"),
                username: contact.username.isEmpty ? nil : contact.username,
                isPlaceholder: stated == nil)
            return ContactListPresentation.matches(item, query: query) ? item : nil
        }
        return accepted + invited
    }

    /// The account keys the list shows for this text: contacts and the people
    /// added through their link. A contact whose name has not arrived is not
    /// among them: they stand under People meanwhile.
    private var listedKeys: Set<Data> {
        ContactRowId.listedKeys(
            shown: items.map(\.id),
            accepted: viewModel.repositoryState.accepted,
            invited: viewModel.shownInvited)
    }

    private var sections: [ContactListPresentation.LetterSection<ContactRowId>] {
        ContactListPresentation.sections(items)
    }

    private var incoming: [IncomingContactRequest] {
        RequestSorting.newestFirst(viewModel.repositoryState.pending, messageId: \.messageId)
    }

    private var sent: [SentContactRequest] {
        RequestSorting.newestFirst(viewModel.repositoryState.sent, messageId: \.messageId)
    }

    private var showsEmptyState: Bool {
        !viewModel.repositoryState.requiresReauth && !isSearching
            && viewModel.repositoryState.accepted.isEmpty
            && viewModel.shownInvited.isEmpty
            && viewModel.repositoryState.pending.isEmpty
            && viewModel.repositoryState.sent.isEmpty
    }

    /// The line when the text matches no contact, for a list that has some.
    private var noMatchText: String? {
        guard items.isEmpty, !viewModel.repositoryState.accepted.isEmpty || !viewModel.shownInvited.isEmpty
        else { return nil }
        return l10n.t("contacts.list.no_match")
    }

    private var showsIndex: Bool {
        !isSearching && sections.count > 1
    }

    // MARK: - Body

    public var body: some View {
        ScrollViewReader { proxy in
            List {
                if viewModel.repositoryState.requiresReauth {
                    Section { ReauthPanel() }
                } else {
                    errorSection
                    if !isSearching {
                        requestsSection
                        sentSection
                    }
                    contactSections
                    if isSearching, let peopleSearch {
                        Section {
                            PeopleSearchSection(
                                model: peopleSearch, includesChannels: false, showsLocalContacts: true,
                                listShowsRows: !items.isEmpty,
                                listed: listedKeys,
                                onOpenPerson: onOpenPerson)
                        }
                        .listRowSeparator(.hidden)
                    } else if isSearching, let noMatchText {
                        Section {
                            Text(noMatchText)
                                .foregroundStyle(Color.lakkoTextMuted)
                                .frame(maxWidth: .infinity)
                        }
                        .listRowSeparator(.hidden)
                    }
                }
            }
            .listStyle(.plain)
            .overlay(alignment: .trailing) {
                if showsIndex {
                    SectionIndexBar(letters: sections.map(\.letter)) { letter in
                        withAnimation { proxy.scrollTo(Self.sectionAnchor(letter), anchor: .top) }
                    }
                }
            }
            .overlay {
                if showsEmptyState {
                    ContentUnavailableView {
                        Label(l10n.t("contacts.list.empty.title"), icon: .personCirclePlus, size: 48, relativeTo: nil)
                    } description: {
                        Text(l10n.t("contacts.list.empty.body"))
                    } actions: {
                        Button(l10n.t("contacts.add.share_invite")) { inviteLinksPresented = true }
                            .buttonStyle(.borderedProminent)
                    }
                }
            }
        }
        // Rebuilt on a layout-direction flip: a UIKit-backed List keeps the
        // mirroring it was built with (see SettingsScreen).
        .id(l10n.layoutDirection)
        .searchable(text: $query, prompt: Text(l10n.t("contacts.list.search")))
        .onChange(of: query) { _, text in peopleSearch?.update(query: text) }
        .refreshable { await viewModel.pullToRefresh() }
        .navigationTitle(l10n.t("nav.contacts"))
        .navigationBarTitleDisplayMode(.large)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                addMenu
            }
        }
        .lakkoActionSheet(isPresented: $addMenuOpen) {
            [
                .action(l10n.t("contacts.add.share_invite"), icon: .link) { inviteLinksPresented = true },
                .action(l10n.t("contacts.scan_qr"), icon: .qrcodeScan) { scanPresented = true },
            ]
        }
        .sheet(isPresented: $inviteLinksPresented) {
            InviteLinksSheet(viewModel: viewModel, onDismiss: { inviteLinksPresented = false })
        }
        .sheet(isPresented: $scanPresented) {
            ScanLinkSheet(
                onLink: { url in
                    scanPresented = false
                    onOpenLink(url)
                },
                onDismiss: { scanPresented = false })
        }
        // The inbox long-poll runs for the whole session in `SessionRoot`,
        // so a chat or group created elsewhere lands whether or not this
        // screen is open. A visit only loads the invite links and asks for
        // one quick fetch so the list is current on arrival.
        .task {
            // The server-side list of live invite links, loaded once per
            // visit so the invite-links sheet opens populated.
            await viewModel.loadActiveInvites()
            await viewModel.pollOnceLongPoll(waitSecs: 0)
        }
    }

    // MARK: - Toolbar

    /// There is no "Add by username" form (`docs/ux/who-can-find-me.md`
    /// section 6.1): a person is found through the search field, which sends
    /// the request from its row.
    ///
    /// Scan QR code reads a Lakko link (`/i/`, `/g/`, `/c/`) someone shows
    /// from their invite-links list and opens it like a tapped link; the
    /// code itself is shown per link in the invite-links sheet.
    ///
    /// There is no call-link entry here (`docs/ux/navigation.md` section 3).
    /// It was put in this menu by the redesign only because Calls had been
    /// removed as a destination; with Calls back, starting a call belongs
    /// there — and the LINK belongs inside the call it leads into
    /// (`docs/ux/call-links.md` section 3), not in a menu about contacts.
    private var addMenu: some View {
        Button { addMenuOpen = true } label: {
            LkIconView(.personPlus, size: 24)
        }
        .accessibilityLabel(l10n.t("contacts.list.add"))
    }

    // MARK: - Sections

    /// The last receive error, when there is one.
    @ViewBuilder
    private var errorSection: some View {
        if let err = viewModel.repositoryState.lastError {
            Section {
                Text(l10n.t(err))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoDanger)
                    .lineLimit(3)
            }
        }
    }

    @ViewBuilder
    private var requestsSection: some View {
        if !incoming.isEmpty {
            Section {
                ForEach(incoming, id: \.messageId) { request in
                    IncomingRequestRow(
                        request: request,
                        viaGroupName: request.viaGroupIdHex.flatMap { viewModel.groupName(groupIdHex: $0) },
                        failed: viewModel.acceptFailed.contains(request.messageId),
                        accepting: viewModel.accepting.contains(request.messageId),
                        onAccept: { viewModel.onAcceptClicked(messageId: request.messageId) },
                        onDecline: { viewModel.onDeclineClicked(messageId: request.messageId) }
                    )
                }
            } header: {
                Text(l10n.t("contacts.list.requests"))
            }
        }
    }

    @ViewBuilder
    private var sentSection: some View {
        if !sent.isEmpty {
            Section {
                DisclosureGroup(isExpanded: $sentExpanded) {
                    ForEach(sent, id: \.messageId) { row in
                        SentRequestRow(
                            request: row,
                            onDismiss: { viewModel.onDismissSentClicked(row) })
                    }
                } label: {
                    Text(l10n.t("contacts.list.sent", texts: ["count": sent.count.formatted(.number.locale(l10n.locale))]))
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Color.lakkoTextMuted)
                }
            }
        }
    }

    private var contactSections: some View {
        ForEach(sections, id: \.letter) { section in
            Section {
                ForEach(section.ids, id: \.self) { id in
                    switch id {
                    case .contact(let messageId):
                        if let contact = contactsById[messageId] {
                            ContactRow(contact: contact)
                        }
                    case .invited(let accountIdHex):
                        if let contact = invitedById[accountIdHex] {
                            InvitedRow(contact: contact)
                        }
                    }
                }
            } header: {
                Text(section.letter)
                    .id(Self.sectionAnchor(section.letter))
            }
        }
    }

    private static func sectionAnchor(_ letter: String) -> String {
        "contacts-section-\(letter)"
    }
}

// MARK: - Rows

/// A request waiting for an answer: the person's name and avatar as found,
/// `@username` under it, Waiting, and **Dismiss**.
private struct SentRequestRow: View {
    @ObservedObject private var l10n = Localization.shared
    let request: SentContactRequest
    let onDismiss: () -> Void
    /// ADR-0084, UX profile-screen section 7 — the photo's video plays while
    /// a pointer hovers the row; `.onHover` never fires for a finger.
    @State private var hovering = false

    var body: some View {
        let line = ContactListPresentation.sentRow(request)
        HStack(spacing: 12) {
            InitialsAvatar(
                name: line.name ?? "",
                identity: request.recipientAccountIdB64,
                size: 40,
                placeholderIcon: .personFill,
                photo: request.recipientKeyB64.flatMap(base64Decode).map { .person($0) },
                playsVideo: hovering
            )
            VStack(alignment: .leading, spacing: 2) {
                Text(line.name ?? l10n.t("contacts.request.someone"))
                    .foregroundStyle(Color.lakkoText)
                if let username = line.username {
                    Text(username)
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
                Text(l10n.t("contacts.list.sent_waiting"))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            Spacer()
            Button(l10n.t("common.dismiss"), action: onDismiss)
                .buttonStyle(.borderless)
                .accessibilityLabel(l10n.t("contacts.list.sent_dismiss"))
        }
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }
}

/// A row of the contact list: an accepted contact, or a person added through
/// their invite link whose app has not answered yet (ADR-0079).
enum ContactRowId: Hashable {
    case contact(Int64)
    case invited(String)

    /// The account keys of the people the rows `shown` stand for.
    static func listedKeys(
        shown: [ContactRowId], accepted: [AcceptedContact], invited: [PendingContact]
    ) -> Set<Data> {
        let shown = Set(shown)
        let contacts = accepted
            .filter { shown.contains(.contact($0.request.messageId)) }
            .map(\.request.fromPubkey)
        let added = invited
            .filter { shown.contains(.invited($0.accountIdHex)) }
            .map(\.signaturePubkey)
        return Set(contacts + added)
    }
}

/// ADR-0079 — a person added through their invite link, before their app has
/// answered: listed like any contact (no "pending" badge), opening the chat
/// with them.
private struct InvitedRow: View {
    @ObservedObject private var l10n = Localization.shared
    let contact: PendingContact
    /// ADR-0084, UX profile-screen section 7 — the photo's video plays while
    /// a pointer hovers the row; `.onHover` never fires for a finger.
    @State private var hovering = false

    var body: some View {
        let stated = PendingContacts.name(contact)
        NavigationLink(value: TabRoute.pendingChat(accountIdHex: contact.accountIdHex)) {
            HStack(spacing: 12) {
                InitialsAvatar(
                    name: stated ?? "",
                    identity: base64Encode(contact.signaturePubkey),
                    size: 40,
                    placeholderIcon: .personFill,
                    photo: .person(contact.signaturePubkey),
                    playsVideo: hovering
                )
                VStack(alignment: .leading, spacing: 2) {
                    Text(stated ?? l10n.t("invite.contact.page.unnamed"))
                        .foregroundStyle(Color.lakkoText)
                    if !contact.username.isEmpty, stated != "@\(contact.username)" {
                        Text("@\(contact.username)")
                            .font(.subheadline)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                }
            }
            .padding(.vertical, 2)
            .contentShape(Rectangle())
            .onHover { hovering = $0 }
        }
    }
}

/// One contact: 40 pt avatar, display name, `@username` under it. Pushes the
/// detail through the tab's route table.
private struct ContactRow: View {
    let contact: AcceptedContact
    /// ADR-0084, UX profile-screen section 7 — the photo's video plays while
    /// a pointer hovers the row; `.onHover` never fires for a finger.
    @State private var hovering = false

    var body: some View {
        ResolvedName(signatureKey: contact.request.fromPubkey) { resolved in
            NavigationLink(value: TabRoute.contactDetail(messageId: contact.request.messageId)) {
                HStack(spacing: 12) {
                    InitialsAvatar(
                        name: resolved.isPlaceholder ? "" : resolved.name,
                        identity: base64Encode(contact.request.fromPubkey),
                        size: 40,
                        placeholderIcon: .personFill,
                        photo: .person(contact.request.fromPubkey),
                        playsVideo: hovering
                    )
                    VStack(alignment: .leading, spacing: 2) {
                        Text(resolved.name)
                            .foregroundStyle(Color.lakkoText)
                        if let line = resolved.usernameLine {
                            Text(line)
                                .font(.subheadline)
                                .foregroundStyle(Color.lakkoTextMuted)
                        }
                    }
                }
                .padding(.vertical, 2)
                .contentShape(Rectangle())
                .onHover { hovering = $0 }
            }
        }
    }
}

/// An incoming request: who is asking (from the profile the request carried,
/// bound to its signed key; "Someone" when nothing is known), their intro, and
/// Accept / Decline.
struct IncomingRequestRow: View {
    @ObservedObject private var l10n = Localization.shared

    /// The first button's word: `Accept`, or `Try again` after an accept that
    /// founded no chat (decided 2026-11-04: no contact appears then, the
    /// request stays with a plain error line, and trying again founds the
    /// chat, which makes the contact and retires the request).
    static func acceptTitleKey(failed: Bool) -> String {
        failed ? "common.try_again" : "contacts.request.accept"
    }

    /// `docs/ux/who-can-find-me.md` 6.4 — the line naming the group a request
    /// came inside: its name, or the unnamed form. `nil` for a mailbox
    /// request.
    static func viaLine(
        viaGroupIdHex: String?, groupName: String?
    ) -> (key: String, texts: [String: String])? {
        guard viaGroupIdHex != nil else { return nil }
        if let groupName = groupName?.trimmingCharacters(in: .whitespacesAndNewlines), !groupName.isEmpty {
            return ("contacts.request.via_group", ["group": groupName])
        }
        return ("contacts.request.via_unnamed_group", [:])
    }

    let request: IncomingContactRequest
    /// The name of the group the request came inside, when it did and the
    /// group has one.
    var viaGroupName: String? = nil
    /// The last Accept failed: the row says so, and Accept works again.
    let failed: Bool
    let accepting: Bool
    let onAccept: () -> Void
    let onDecline: () -> Void
    /// ADR-0084, UX profile-screen section 7 — the photo's video plays while
    /// a pointer hovers the row; `.onHover` never fires for a finger.
    @State private var hovering = false

    var body: some View {
        ResolvedName(signatureKey: request.fromPubkey) { resolved in
            HStack(alignment: .top, spacing: 12) {
                InitialsAvatar(
                    name: resolved.isPlaceholder ? "" : resolved.name,
                    identity: base64Encode(request.fromPubkey),
                    size: 40,
                    placeholderIcon: .personFill,
                    photo: .person(request.fromPubkey),
                    playsVideo: hovering
                )
                VStack(alignment: .leading, spacing: 6) {
                    Text(resolved.isPlaceholder ? l10n.t("contacts.request.someone") : resolved.name)
                        .foregroundStyle(Color.lakkoText)
                    if let line = resolved.usernameLine {
                        Text(line)
                            .font(.subheadline)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    if let via = Self.viaLine(viaGroupIdHex: request.viaGroupIdHex, groupName: viaGroupName) {
                        Text(l10n.t(via.key, texts: via.texts))
                            .font(.subheadline)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    if let intro = request.intro?.trimmingCharacters(in: .whitespacesAndNewlines),
                       !intro.isEmpty {
                        Text(intro)
                            .font(.subheadline)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    HStack(spacing: 8) {
                        Button(action: onAccept) {
                            Text(l10n.t(Self.acceptTitleKey(failed: failed))).frame(minWidth: 72)
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                        .disabled(accepting)
                        Button(action: onDecline) {
                            Text(l10n.t("contacts.request.decline")).frame(minWidth: 72)
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .disabled(accepting)
                    }
                    .padding(.top, 2)
                    if failed {
                        Text(l10n.t("contacts.request.accept_failed"))
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoDanger)
                            .accessibilityAddTraits(.updatesFrequently)
                    }
                }
            }
            .padding(.vertical, 4)
            .contentShape(Rectangle())
            .onHover { hovering = $0 }
        }
    }
}

/// The section index down the right edge: tap or drag to jump to a letter.
private struct SectionIndexBar: View {
    let letters: [String]
    let onSelect: (String) -> Void

    private let rowHeight: CGFloat = 15
    @State private var lastSelected: String?

    var body: some View {
        VStack(spacing: 0) {
            ForEach(letters, id: \.self) { letter in
                Text(letter)
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(Color.lakkoAccent)
                    .frame(width: 18, height: rowHeight)
            }
        }
        .padding(.vertical, 4)
        .contentShape(Rectangle())
        .gesture(
            DragGesture(minimumDistance: 0)
                .onChanged { value in
                    let index = Int((value.location.y - 4) / rowHeight)
                    guard letters.indices.contains(index) else { return }
                    let letter = letters[index]
                    if letter != lastSelected {
                        lastSelected = letter
                        onSelect(letter)
                    }
                }
                .onEnded { _ in lastSelected = nil }
        )
        .padding(.trailing, 2)
        .accessibilityLabel(Localization.text("contacts.list.section_index"))
    }
}

private struct ReauthPanel: View {
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(l10n.t("chat.reauth.title"))
                .font(.headline)
            Text(l10n.t("contacts.reauth.body"))
                .font(.body)
                .foregroundStyle(Color.lakkoTextMuted)
        }
        .padding(.vertical, 8)
    }
}

/// Contacts > Scan QR code: the camera reads codes until one is a Lakko link,
/// which is then opened through the app's link route; any other code shows
/// a line and the camera keeps scanning.
private struct ScanLinkSheet: View {
    let onLink: (URL) -> Void
    let onDismiss: () -> Void
    @ObservedObject private var l10n = Localization.shared
    @State private var notALink = false
    /// A catalog key of a camera failure, when the camera could not start.
    @State private var cameraErrorKey: String?

    var body: some View {
        NavigationStack {
            VStack(spacing: 16) {
                if let cameraErrorKey {
                    Text(l10n.t(cameraErrorKey))
                        .font(.callout)
                        .foregroundStyle(Color.lakkoDanger)
                        .multilineTextAlignment(.center)
                        .padding()
                } else {
                    QrScannerView(
                        onDetected: { text in
                            if let url = InviteIntake.scannedLink(text) { onLink(url) }
                        },
                        onError: { key in cameraErrorKey = key },
                        accept: { InviteIntake.scannedLink($0) != nil },
                        onRejected: { _ in
                            if !notALink { notALink = true }
                        })
                        .aspectRatio(1, contentMode: .fit)
                        .clipShape(RoundedRectangle(cornerRadius: 16))
                        .padding(.horizontal, 16)
                    if notALink {
                        Text(l10n.t("contacts.scan_qr.not_a_link"))
                            .font(.callout)
                            .foregroundStyle(Color.lakkoDanger)
                            .multilineTextAlignment(.center)
                            .padding(.horizontal, 24)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(.top, 16)
            .navigationTitle(l10n.t("contacts.scan_qr"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button(l10n.t("common.cancel"), action: onDismiss)
                }
            }
        }
        .presentationDetents([.large])
    }
}
