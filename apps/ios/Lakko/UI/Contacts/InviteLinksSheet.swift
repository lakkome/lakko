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
    /// ADR-0058 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    /// Frozen per body so every row's "expires in" agrees.
    @State private var now = Date()

    private var rows: [InviteLinkRow] {
        InviteLinkRows.merge(
            active: viewModel.activeInvites,
            sessionGenerated: viewModel.repositoryState.outstandingInvites,
            now: now)
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
                                Text(l10n.t("chats.new_group.creating"))
                            } else {
                                Label(l10n.t("invite.links.create"), systemImage: "link.badge.plus")
                            }
                        }
                    }
                    .disabled(viewModel.generatingInvite)
                    if let error = viewModel.inviteError {
                        Text(error.text)
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoDanger)
                    }
                } footer: {
                    Text(l10n.t("invite.links.desc"))
                }

                Section {
                    if let error = viewModel.activeInvitesError {
                        Text(error.text)
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoDanger)
                    }
                    if !viewModel.activeInvitesLoaded && rows.isEmpty {
                        HStack(spacing: 8) {
                            if viewModel.loadingActiveInvites {
                                ProgressView().controlSize(.small)
                            }
                            Text(l10n.t("common.loading"))
                                .foregroundStyle(Color.lakkoTextMuted)
                        }
                    } else if rows.isEmpty {
                        Text(l10n.t("invite.links.empty"))
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
                    Text(l10n.t("invite.links.list"))
                }
            }
            .listStyle(.insetGrouped)
            .navigationTitle(l10n.t("contacts.invite_links.title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button(l10n.t("common.done"), action: onDismiss)
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
            let date = created.formatted(
                Date.FormatStyle(date: .abbreviated, time: .shortened).locale(l10n.locale))
            lines.append(l10n.t("invite.links.created", texts: ["date": date]))
        }
        var status = InviteLinkRows.expiryCopy(expiryMs: row.expiryMs, now: now)
        if let uses = row.usesLeft {
            status = l10n.t("common.fact_pair", texts: ["first": status, "second": uses])
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
    let url: String
    var highlighted: Bool = false
    var revoking: Bool = false
    var revoke: (() -> Void)? = nil

    @State private var copied = false
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Text(title)
                    .font(.body.weight(.medium))
                    .foregroundStyle(Color.lakkoText)
                if highlighted {
                    Text(l10n.t("invite.links.new_badge"))
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
                Button {
                    UIPasteboard.general.string = url
                    copied = true
                    Task { @MainActor in
                        try? await Task.sleep(nanoseconds: 1_500_000_000)
                        copied = false
                    }
                } label: {
                    Label(l10n.t(copied ? "common.copied" : "common.copy"), systemImage: copied ? "checkmark" : "doc.on.doc")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)

                ShareLink(item: url) {
                    Label(l10n.t("chat.invite_link.share"), systemImage: "square.and.arrow.up")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)

                if let revoke {
                    Spacer()
                    Button(role: .destructive, action: revoke) {
                        if revoking {
                            ProgressView().controlSize(.small)
                        } else {
                            Label(l10n.t("chat.invite_link.revoke"), systemImage: "trash")
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
