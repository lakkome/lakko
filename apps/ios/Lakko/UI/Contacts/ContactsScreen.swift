// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The Contacts tab (docs/ux/client-redesign.md section 5.3): a searchable
/// list with the incoming requests on top (Accept / Decline on the row), the
/// sent requests under a collapsed "Sent" header, then the contacts in
/// alphabetical sections with an index on the right. The add menu holds
/// "Add by username", "Share invite link" and "Create call link"; a row pushes
/// ``ContactDetailScreen``.
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

    /// ADR-0032 call links — the session the owner signs the capability with,
    /// and the intake that routes "Start call" into the `/c/` owner host flow.
    @EnvironmentObject private var authRepository: AuthRepository
    @EnvironmentObject private var inviteIntake: InviteIntake
    /// ADR-0048 — names resolve through the directory; observing it here
    /// regroups the list in place when a profile or a verdict arrives.
    @ObservedObject private var profiles = ProfileDirectory.shared

    @State private var query = ""
    @State private var addContactPresented = false
    @State private var inviteLinksPresented = false
    @State private var createCallLinkPresented = false
    @State private var sentExpanded = false

    public init(viewModel: ContactsViewModel) {
        self.viewModel = viewModel
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

    /// The contacts as the grouping sees them, already filtered by the search.
    private var items: [ContactListPresentation.Item<Int64>] {
        // Reading `revision` subscribes this body to every profile change.
        _ = profiles.revision
        return viewModel.repositoryState.accepted.compactMap { contact in
            let resolved = profiles.resolve(signatureKey: contact.request.fromPubkey)
            let item = ContactListPresentation.Item(
                id: contact.request.messageId,
                name: resolved.name,
                username: resolved.verifiedUsername,
                isPlaceholder: resolved.isPlaceholder)
            return ContactListPresentation.matches(item, query: query) ? item : nil
        }
    }

    private var sections: [ContactListPresentation.LetterSection<Int64>] {
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
            && viewModel.repositoryState.pending.isEmpty
            && viewModel.repositoryState.sent.isEmpty
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
                        Label("No contacts yet", systemImage: "person.crop.circle.badge.plus")
                    } description: {
                        Text("People you add appear here.")
                    } actions: {
                        Button("Add contact") { addContactPresented = true }
                            .buttonStyle(.borderedProminent)
                    }
                } else if isSearching && items.isEmpty {
                    ContentUnavailableView.search(text: query)
                }
            }
        }
        .searchable(text: $query, prompt: "Search contacts")
        .refreshable { await viewModel.pullToRefresh() }
        .navigationTitle("Contacts")
        .navigationBarTitleDisplayMode(.large)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                addMenu
            }
        }
        .sheet(isPresented: $addContactPresented) {
            AddContactSheet(viewModel: viewModel, onDismiss: { addContactPresented = false })
        }
        .sheet(isPresented: $inviteLinksPresented) {
            InviteLinksSheet(viewModel: viewModel, onDismiss: { inviteLinksPresented = false })
        }
        .sheet(isPresented: $createCallLinkPresented) {
            CreateCallLinkSheet(
                authRepository: authRepository,
                onDismiss: { createCallLinkPresented = false },
                onStartCall: { url in
                    createCallLinkPresented = false
                    // Park the link exactly as a tapped `/c/<slug>` would:
                    // the landing recognises this device as the owner and
                    // offers "Start call" (the owner host flow).
                    if let parsed = URL(string: url) {
                        inviteIntake.handle(parsed)
                    }
                }
            )
        }
        // Foreground continuous polling (1.14e5). The `while !Task.isCancelled`
        // loop runs while the screen is on-screen and cancels on disappear.
        // Long-poll budget = 30s (server-clamped); on each server-reported
        // batch we loop immediately, so a dropped connection / catch-up cursor
        // lands on the UI within at most ~30s + a round-trip.
        .task {
            // The server-side list of live invite links, loaded once per
            // visit so the invite-links sheet opens populated.
            await viewModel.loadActiveInvites()
            while !Task.isCancelled {
                await viewModel.pollOnceLongPoll()
                // No sleep: the long-poll holds the connection itself.
                // `Task.yield` gives the runloop a tick so a tight error
                // loop doesn't starve the main actor.
                await Task.yield()
            }
        }
    }

    // MARK: - Toolbar

    /// Show / scan a contact QR code are not offered: no contact-QR flow
    /// exists in the repository (the QR renderer and scanner serve device
    /// pairing only).
    private var addMenu: some View {
        Menu {
            Button { addContactPresented = true } label: {
                Label("Add by username", systemImage: "at")
            }
            Button { inviteLinksPresented = true } label: {
                Label("Share invite link", systemImage: "link")
            }
            Button { createCallLinkPresented = true } label: {
                Label("Create call link", systemImage: "phone.badge.plus")
            }
        } label: {
            Image(systemName: "person.badge.plus")
        }
        .accessibilityLabel("Add")
    }

    // MARK: - Sections

    /// The last receive error, when there is one.
    @ViewBuilder
    private var errorSection: some View {
        if let err = viewModel.repositoryState.lastError {
            Section {
                Text(err)
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
                        onAccept: { viewModel.onAcceptClicked(messageId: request.messageId) },
                        onDecline: { viewModel.onDeclineClicked(messageId: request.messageId) }
                    )
                }
            } header: {
                Text("Requests")
            }
        }
    }

    @ViewBuilder
    private var sentSection: some View {
        if !sent.isEmpty {
            Section {
                DisclosureGroup(isExpanded: $sentExpanded) {
                    ForEach(sent, id: \.messageId) { row in
                        HStack {
                            Text("@\(row.recipientUsername)")
                                .foregroundStyle(Color.lakkoText)
                            Spacer()
                            Text("Waiting")
                                .font(.footnote)
                                .foregroundStyle(Color.lakkoTextMuted)
                        }
                    }
                } label: {
                    Text("Sent (\(sent.count))")
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
                    if let contact = contactsById[id] {
                        ContactRow(contact: contact)
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

/// One contact: 40 pt avatar, display name, `@username` under it. Pushes the
/// detail through the tab's route table.
private struct ContactRow: View {
    let contact: AcceptedContact

    var body: some View {
        ResolvedName(signatureKey: contact.request.fromPubkey) { resolved in
            NavigationLink(value: TabRoute.contactDetail(messageId: contact.request.messageId)) {
                HStack(spacing: 12) {
                    InitialsAvatar(
                        name: resolved.isPlaceholder ? "" : resolved.name,
                        identity: base64Encode(contact.request.fromPubkey),
                        size: 40,
                        placeholderSymbol: "person.fill"
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
            }
        }
    }
}

/// An incoming request: who is asking (from the profile the request carried,
/// bound to its signed key; "Someone" when nothing is known), their intro, and
/// Accept / Decline.
private struct IncomingRequestRow: View {
    let request: IncomingContactRequest
    let onAccept: () -> Void
    let onDecline: () -> Void

    var body: some View {
        ResolvedName(signatureKey: request.fromPubkey) { resolved in
            HStack(alignment: .top, spacing: 12) {
                InitialsAvatar(
                    name: resolved.isPlaceholder ? "" : resolved.name,
                    identity: base64Encode(request.fromPubkey),
                    size: 40,
                    placeholderSymbol: "person.fill"
                )
                VStack(alignment: .leading, spacing: 6) {
                    Text(resolved.isPlaceholder ? ResolvedDisplayName.placeholderSomeone : resolved.name)
                        .foregroundStyle(Color.lakkoText)
                    if let line = resolved.usernameLine {
                        Text(line)
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
                            Text("Accept").frame(minWidth: 72)
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                        Button(action: onDecline) {
                            Text("Decline").frame(minWidth: 72)
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    }
                    .padding(.top, 2)
                }
            }
            .padding(.vertical, 4)
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
        .accessibilityLabel("Section index")
    }
}

private struct ReauthPanel: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Re-enter your password")
                .font(.headline)
            Text("Log out and log back in to send and receive contact requests.")
                .font(.body)
                .foregroundStyle(Color.lakkoTextMuted)
        }
        .padding(.vertical, 8)
    }
}
