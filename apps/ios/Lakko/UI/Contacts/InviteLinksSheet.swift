// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// Contacts > Share invite link (UX spec section 5.3): the active links with
/// Copy, Share and Revoke, and "Create link". The server list
/// (`listActiveInvites`) is the truth; links generated in this session that
/// the server list does not show yet are merged in (``InviteLinkRows/merge``).
/// A raw URL is never shown in a text field — a row is a title, the dates and
/// the actions.
struct InviteLinksSheet: View {
    @ObservedObject var viewModel: ContactsViewModel
    let onDismiss: () -> Void

    /// Frozen per body so every row's "expires in" agrees.
    @State private var now = Date()

    private var rows: [InviteLinkRow] {
        InviteLinkRows.merge(
            active: viewModel.activeInvites,
            sessionGenerated: viewModel.repositoryState.outstandingInvites,
            now: now,
            accountIdB64: viewModel.inviteAccountIdB64)
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Button {
                        viewModel.onGenerateInviteClicked()
                    } label: {
                        HStack {
                            if viewModel.generatingInvite {
                                ProgressView().controlSize(.small)
                                Text("Creating link…")
                            } else {
                                Label("Create link", systemImage: "link.badge.plus")
                            }
                        }
                    }
                    .disabled(viewModel.generatingInvite)
                    if let error = viewModel.inviteError {
                        Text(error)
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoDanger)
                    }
                } footer: {
                    Text(
                        "A link works once and for 7 days. Whoever opens it is added "
                            + "as your contact without seeing your username first."
                    )
                }

                Section {
                    if let error = viewModel.activeInvitesError {
                        Text(error)
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoDanger)
                    }
                    if !viewModel.activeInvitesLoaded && rows.isEmpty {
                        HStack(spacing: 8) {
                            if viewModel.loadingActiveInvites {
                                ProgressView().controlSize(.small)
                            }
                            Text("Loading...")
                                .foregroundStyle(Color.lakkoTextMuted)
                        }
                    } else if rows.isEmpty {
                        Text("No active links.")
                            .foregroundStyle(Color.lakkoTextMuted)
                    } else {
                        ForEach(rows) { row in
                            LinkActionsRow(
                                title: row.title,
                                details: details(for: row),
                                url: row.url,
                                highlighted: row.slug == viewModel.lastGeneratedInvite?.blobIdSlug,
                                revoking: viewModel.revokingActiveInviteSlug == row.slug,
                                revoke: { revoke(row) }
                            )
                            .disabled(
                                viewModel.revokingActiveInviteSlug != nil
                                    && viewModel.revokingActiveInviteSlug != row.slug)
                        }
                    }
                } header: {
                    Text("Active links")
                }
            }
            .listStyle(.insetGrouped)
            .navigationTitle("Invite links")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done", action: onDismiss)
                }
            }
        }
        .task {
            now = Date()
            await viewModel.loadActiveInvites()
        }
        .presentationDetents([.large])
    }

    private func details(for row: InviteLinkRow) -> [String] {
        var lines: [String] = []
        if let created = row.createdAt {
            lines.append("Created \(created.formatted(date: .abbreviated, time: .shortened))")
        }
        var status = InviteLinkRows.expiryCopy(expiryMs: row.expiryMs, now: now)
        if let uses = row.usesLeft {
            status += " · \(uses)"
        }
        lines.append(status)
        return lines
    }

    private func revoke(_ row: InviteLinkRow) {
        switch row.origin {
        case .server:
            viewModel.onRevokeActiveInviteClicked(slug: row.slug)
        case .session(let blobId):
            viewModel.onRevokeInviteClicked(blobId: blobId)
        }
    }
}

/// One shareable link as a row: title, detail lines, then Copy, Share and
/// (when offered) Revoke. Shared by the invite-links sheet and the call-link
/// result, so a link always looks the same and the URL itself stays out of
/// the way.
struct LinkActionsRow: View {
    let title: String
    var details: [String] = []
    /// `nil` when the link's secret is not on this device (a link another
    /// device minted, ADR-0028 amendment 2026-09-28): Revoke only.
    let url: String?
    var highlighted: Bool = false
    var revoking: Bool = false
    var revoke: (() -> Void)? = nil

    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Text(title)
                    .font(.body.weight(.medium))
                    .foregroundStyle(Color.lakkoText)
                if highlighted {
                    Text("New")
                        .font(.caption2.weight(.semibold))
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Color.lakkoAccentSoft)
                        .foregroundStyle(Color.lakkoAccent)
                        .clipShape(Capsule())
                }
            }
            ForEach(details, id: \.self) { line in
                Text(line)
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            HStack(spacing: 8) {
                if let url {
                    Button {
                        UIPasteboard.general.string = url
                        copied = true
                        Task { @MainActor in
                            try? await Task.sleep(nanoseconds: 1_500_000_000)
                            copied = false
                        }
                    } label: {
                        Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)

                    ShareLink(item: url) {
                        Label("Share", systemImage: "square.and.arrow.up")
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                } else {
                    Text(InviteLinkRows.createdElsewhereCopy)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                }

                if let revoke {
                    Spacer()
                    Button(role: .destructive, action: revoke) {
                        if revoking {
                            ProgressView().controlSize(.small)
                        } else {
                            Label("Revoke", systemImage: "trash")
                        }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(revoking)
                }
            }
        }
        .padding(.vertical, 4)
    }
}
