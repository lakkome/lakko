// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// SwiftUI Contacts screen.
///
/// Mirror of:
///
///   * `apps/web/src/contacts/ContactsRoute.tsx` (1.17e6)
///   * `apps/android/.../ui/contacts/ContactsScreen.kt` (1.15e6)
///
/// **Phase 1.14e4 scope**: send form + pending list with Accept /
/// Decline + sent log + accepted log + Refresh action. Invite
/// generate UI lands in 1.14e7; continuous polling
/// `LaunchedEffect`-equivalent lands in 1.14e5.
///
/// `onBack` is the up-affordance back to ``HomeScreen``; the
/// `AppRoot`-level router observes the
/// ``SessionDestination`` toggle (1.14e7 adds the multi-destination
/// session shell — for 1.14e4 we keep the existing two-state
/// home/contacts toggle simple).
public struct ContactsScreen: View {

    @ObservedObject public var viewModel: ContactsViewModel
    public var onBack: () -> Void

    public init(viewModel: ContactsViewModel, onBack: @escaping () -> Void) {
        self.viewModel = viewModel
        self.onBack = onBack
    }

    public var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    if viewModel.repositoryState.requiresReauth {
                        ReauthPanel()
                    } else {
                        sendForm
                        actionsRow
                        inviteSection
                        pendingSection
                        acceptedSection
                        sentSection
                    }
                }
                .padding(24)
            }
            .sheet(item: invitePresentationBinding) { invite in
                GeneratedInviteSheet(invite: invite) {
                    viewModel.dismissGeneratedInvite()
                }
            }
            .navigationTitle("Contacts")
            .navigationBarTitleDisplayMode(.inline)
            // Foreground continuous polling (1.14e5). SwiftUI's
            // `.task { … }` modifier ties the lifecycle of the
            // task to the view: the `while !Task.isCancelled`
            // loop runs while the screen is on-screen + cancels
            // automatically on `onDisappear`. Mirror of the
            // Compose `LaunchedEffect(Unit) { while (isActive) }`
            // pattern from Android 1.15e7. Long-poll budget = 30s
            // (server-clamped to its `MAX_WAIT_SECS`); on each
            // server-reported batch we loop immediately, so a
            // dropped connection / catch-up cursor lands on the
            // UI within at most ~30s + a round-trip.
            .task {
                while !Task.isCancelled {
                    await viewModel.pollOnceLongPoll()
                    // No sleep: the long-poll holds the
                    // connection for up to `waitSecs` itself.
                    // On an immediate-return path (server has
                    // pending events) we want to loop back into
                    // the next poll without delay. `Task.yield`
                    // gives the runloop a tick so a tight error
                    // loop doesn't starve the main actor.
                    await Task.yield()
                }
            }
            .toolbar {
                ToolbarItem(placement: .navigationBarLeading) {
                    Button(action: onBack) {
                        Label("Home", systemImage: "chevron.backward")
                    }
                }
            }
        }
    }

    // MARK: - Sections

    private var sendForm: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Add contact")
                .font(.headline)
            TextField("@username", text: $viewModel.usernameInput)
                .textFieldStyle(.roundedBorder)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled(true)
                .disabled(viewModel.sending)
            TextField("Optional intro", text: $viewModel.introInput, axis: .vertical)
                .textFieldStyle(.roundedBorder)
                .lineLimit(2...4)
                .disabled(viewModel.sending)

            if let err = viewModel.sendError {
                Text(err)
                    .font(.footnote)
                    .foregroundStyle(.red)
            }

            Button(action: viewModel.onSendClicked) {
                HStack {
                    if viewModel.sending {
                        ProgressView()
                            .controlSize(.small)
                            .progressViewStyle(.circular)
                            .tint(.white)
                        Text("Sending…")
                    } else {
                        Text("Send contact request")
                    }
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(viewModel.sending)
        }
    }

    private var actionsRow: some View {
        HStack {
            Button(action: viewModel.onRefreshClicked) {
                HStack {
                    if viewModel.repositoryState.refreshing {
                        ProgressView().controlSize(.small)
                    } else {
                        Image(systemName: "arrow.clockwise")
                    }
                    Text("Refresh inbox")
                }
            }
            .buttonStyle(.bordered)
            .disabled(viewModel.repositoryState.refreshing)
            Spacer()
            if let err = viewModel.repositoryState.lastError {
                Text(err)
                    .font(.footnote)
                    .foregroundStyle(.red)
                    .lineLimit(2)
            }
        }
    }

    @ViewBuilder
    private var pendingSection: some View {
        if !viewModel.repositoryState.pending.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Text("Pending (\(viewModel.repositoryState.pending.count))")
                    .font(.headline)
                ForEach(viewModel.repositoryState.pending, id: \.messageId) { req in
                    PendingRequestRow(
                        request: req,
                        onAccept: { viewModel.onAcceptClicked(messageId: req.messageId) },
                        onDecline: { viewModel.onDeclineClicked(messageId: req.messageId) }
                    )
                }
            }
        }
    }

    @ViewBuilder
    private var acceptedSection: some View {
        if !viewModel.repositoryState.accepted.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Text("Accepted (\(viewModel.repositoryState.accepted.count))")
                    .font(.headline)
                ForEach(
                    viewModel.repositoryState.accepted,
                    id: \.request.messageId
                ) { entry in
                    AcceptedContactRow(entry: entry)
                }
            }
        }
    }

    /// Binding adapter — SwiftUI's `.sheet(item:)` wants an
    /// `Identifiable` payload. ``GeneratedInvite`` isn't
    /// `Identifiable` directly (would need a stable `id`); wrap
    /// it in a small adapter that pulls the slug as the
    /// identifier.
    private var invitePresentationBinding: Binding<IdentifiableInvite?> {
        Binding(
            get: {
                viewModel.lastGeneratedInvite.map(IdentifiableInvite.init)
            },
            set: { newValue in
                if newValue == nil {
                    viewModel.dismissGeneratedInvite()
                }
            }
        )
    }

    @ViewBuilder
    private var inviteSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("Invite links")
                    .font(.headline)
                Spacer()
                Button(action: viewModel.onGenerateInviteClicked) {
                    HStack {
                        if viewModel.generatingInvite {
                            ProgressView().controlSize(.small)
                        } else {
                            Image(systemName: "link.badge.plus")
                        }
                        Text("Generate link")
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(viewModel.generatingInvite)
            }

            if let err = viewModel.inviteError {
                Text(err)
                    .font(.footnote)
                    .foregroundStyle(.red)
            }

            if viewModel.repositoryState.outstandingInvites.isEmpty {
                Text(
                    "Share a one-time 7-day link to add a contact without " +
                    "exposing your username on the wire."
                )
                .font(.footnote)
                .foregroundStyle(.secondary)
            } else {
                ForEach(
                    viewModel.repositoryState.outstandingInvites,
                    id: \.blobIdSlug
                ) { invite in
                    OutstandingInviteRow(
                        invite: invite,
                        onRevoke: {
                            viewModel.onRevokeInviteClicked(blobId: invite.blobId)
                        }
                    )
                }
            }
        }
    }

    @ViewBuilder
    private var sentSection: some View {
        if !viewModel.repositoryState.sent.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Text("Sent (\(viewModel.repositoryState.sent.count))")
                    .font(.headline)
                ForEach(
                    viewModel.repositoryState.sent,
                    id: \.messageId
                ) { row in
                    HStack {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("@\(row.recipientUsername)")
                                .font(.body)
                            Text("waiting for accept…")
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                    }
                    .padding(.vertical, 4)
                }
            }
        }
    }
}

// MARK: - Row components

private struct PendingRequestRow: View {
    let request: IncomingContactRequest
    let onAccept: () -> Void
    let onDecline: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            // The sender's pubkey hex is the most reliable handle
            // we have until 1.14e+ wires reverse-lookup from
            // pubkey → username via discovery.
            Text(shortFingerprint(request.fromPubkey))
                .font(.system(.body, design: .monospaced))
                .textSelection(.enabled)
            if let intro = request.intro, !intro.isEmpty {
                Text(intro)
                    .font(.body)
                    .foregroundStyle(.secondary)
            }
            HStack(spacing: 8) {
                Button(action: onAccept) {
                    Text("Accept")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                Button(action: onDecline) {
                    Text("Decline")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
        }
        .padding(12)
        .background(Color(.secondarySystemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    /// Short hex fingerprint — first 8 bytes of the 32-byte
    /// pubkey, formatted as `xxxx:xxxx:xxxx:xxxx`. Same surface
    /// the Android side renders so the user has a stable visual
    /// hook to compare across devices (OOB verification lands
    /// post-MVP).
    private func shortFingerprint(_ pubkey: Data) -> String {
        let bytes = pubkey.prefix(8)
        let hex = bytes.map { String(format: "%02x", $0) }.joined()
        var out = ""
        for (i, ch) in hex.enumerated() {
            if i > 0 && i % 4 == 0 { out.append(":") }
            out.append(ch)
        }
        return out
    }
}

private struct AcceptedContactRow: View {
    let entry: AcceptedContact

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 4) {
                Text(shortFingerprint(entry.request.fromPubkey))
                    .font(.system(.body, design: .monospaced))
                if entry.groupIdHex == nil {
                    Text("Open chat (coming in 1.14f)")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                } else {
                    Text("Chat ready")
                        .font(.footnote)
                        .foregroundStyle(.green)
                }
            }
            Spacer()
        }
        .padding(.vertical, 4)
    }

    private func shortFingerprint(_ pubkey: Data) -> String {
        pubkey.prefix(8).map { String(format: "%02x", $0) }.joined()
    }
}

// MARK: - Invite components

/// SwiftUI `Identifiable` adapter for ``GeneratedInvite`` so a
/// `.sheet(item:)` modifier can drive presentation. The slug is
/// unique per upload (server-side content-addressed by
/// SHA-256(capability)) so it doubles as the identity.
private struct IdentifiableInvite: Identifiable {
    let invite: GeneratedInvite
    var id: String { invite.blobIdSlug }
    init(_ invite: GeneratedInvite) { self.invite = invite }
}

private struct GeneratedInviteSheet: View {
    let invite: GeneratedInvite
    let onDismiss: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text("Invite link ready")
                    .font(.title2)
                    .fontWeight(.semibold)
                Spacer()
                Button("Done", action: onDismiss)
            }

            Text(
                "Share this link with the person you want to add. They " +
                "can tap it on any Lakko-installed device to accept the " +
                "invite without exposing your username."
            )
            .font(.body)
            .foregroundStyle(.secondary)

            Text(invite.url)
                .font(.system(.body, design: .monospaced))
                .textSelection(.enabled)
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color(.secondarySystemBackground))
                .clipShape(RoundedRectangle(cornerRadius: 8))

            HStack(spacing: 12) {
                Button(action: {
                    UIPasteboard.general.string = invite.url
                }) {
                    Label("Copy", systemImage: "doc.on.doc")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)

                ShareLink(item: invite.url) {
                    Label("Share", systemImage: "square.and.arrow.up")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .controlSize(.large)
            }

            Spacer()
        }
        .padding(24)
        .presentationDetents([.medium, .large])
    }
}

private struct OutstandingInviteRow: View {
    let invite: GeneratedInvite
    let onRevoke: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            VStack(alignment: .leading, spacing: 4) {
                Text(invite.url)
                    .font(.system(.footnote, design: .monospaced))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                if invite.label.isEmpty {
                    Text(expiryCopy)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } else {
                    Text("\(invite.label) · \(expiryCopy)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Spacer()
            Button(action: onRevoke) {
                Image(systemName: "trash")
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
        }
        .padding(.vertical, 4)
    }

    private var expiryCopy: String {
        let expiry = Date(timeIntervalSince1970: TimeInterval(invite.expiryMs) / 1000)
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .short
        return "expires \(formatter.localizedString(for: expiry, relativeTo: Date()))"
    }
}

private struct ReauthPanel: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Re-enter your password")
                .font(.headline)
            Text(
                "Sending and reading contact requests needs the in-memory " +
                "export key from your last login. Log out and log back in " +
                "to repopulate it."
            )
            .font(.body)
            .foregroundStyle(.secondary)
        }
        .padding(16)
        .background(Color(.secondarySystemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }
}
