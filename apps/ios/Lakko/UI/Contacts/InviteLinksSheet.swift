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
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    /// Frozen per body so every row's "expires in" agrees.
    @State private var now = Date()
    /// The link whose QR code is on screen, when one is.
    @State private var qrLink: QrLink?
    /// The create form is open.
    @State private var creating = false
    /// What the create form holds; reset each time it opens.
    @State private var form = InviteCreateForm()

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
                if creating {
                    createForm
                } else {
                    Section {
                        Button {
                            form = InviteCreateForm()
                            viewModel.clearInviteError()
                            creating = true
                        } label: {
                            Label(l10n.t("invite.links.create"), systemImage: "link.badge.plus")
                                .foregroundStyle(Color.lakkoAccent)
                        }
                        if let error = viewModel.inviteError {
                            errorLine(error.text)
                        }
                    } footer: {
                        Text(l10n.t("invite.links.desc"))
                    }
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
                                revoke: { revoke(row) },
                                showQr: row.url.map { url in { qrLink = QrLink(url: url) } }
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
        .sheet(item: $qrLink) { link in
            LinkQrSheet(url: link.url, onDismiss: { qrLink = nil })
        }
        .task {
            now = Date()
            await viewModel.loadActiveInvites()
        }
        .presentationDetents([.large])
    }

    // MARK: - Create form

    /// Label, Expires and Uses (the web form's choices and bounds), then
    /// Create and Cancel. Create stays disabled while a custom field does not
    /// hold a valid value.
    @ViewBuilder
    private var createForm: some View {
        Section {
            TextField(l10n.t("invite.links.label_placeholder"), text: labelBinding)
                .textInputAutocapitalization(.sentences)
                .disabled(viewModel.generatingInvite)
        } header: {
            Text(l10n.t("invite.links.label_field"))
        }

        Section {
            Picker(l10n.t("invite.links.expires_field"), selection: $form.expiry) {
                Text(l10n.t("invite.links.expires_option_days", counts: ["count": 1]))
                    .tag(InviteCreateForm.Expiry.oneDay)
                Text(l10n.t("invite.links.expires_option_days", counts: ["count": 7]))
                    .tag(InviteCreateForm.Expiry.sevenDays)
                Text(l10n.t("invite.links.expires_option_days", counts: ["count": 30]))
                    .tag(InviteCreateForm.Expiry.thirtyDays)
                Text(l10n.t("invite.links.expires_option_never"))
                    .tag(InviteCreateForm.Expiry.never)
                Text(l10n.t("invite.links.expires_option_custom"))
                    .tag(InviteCreateForm.Expiry.custom)
            }
            .pickerStyle(.inline)
            .labelsHidden()
            .tint(Color.lakkoAccent)
            if form.expiry == .custom {
                numberField(
                    title: l10n.t("invite.links.custom_expiry_amount"), text: $form.customExpiryAmount)
                Picker(l10n.t("invite.links.custom_expiry_amount"), selection: $form.customExpiryUnit) {
                    Text(l10n.t("invite.links.unit_hours")).tag(InviteCreateForm.ExpiryUnit.hours)
                    Text(l10n.t("invite.links.unit_days")).tag(InviteCreateForm.ExpiryUnit.days)
                }
                .pickerStyle(.segmented)
                .disabled(viewModel.generatingInvite)
                if form.showsCustomExpiryError {
                    errorLine(l10n.t("invite.links.error.custom_expiry"))
                }
            }
        } header: {
            Text(l10n.t("invite.links.expires_field"))
        }
        .disabled(viewModel.generatingInvite)

        Section {
            Picker(l10n.t("invite.links.uses_field"), selection: $form.uses) {
                Text(l10n.t("invite.links.uses_option_one")).tag(InviteCreateForm.Uses.one)
                Text(l10n.t("invite.links.uses_option_up_to", counts: ["count": 10]))
                    .tag(InviteCreateForm.Uses.upToTen)
                Text(l10n.t("invite.links.uses_option_unlimited")).tag(InviteCreateForm.Uses.unlimited)
                Text(l10n.t("invite.links.uses_option_custom")).tag(InviteCreateForm.Uses.custom)
            }
            .pickerStyle(.inline)
            .labelsHidden()
            .tint(Color.lakkoAccent)
            if form.uses == .custom {
                numberField(title: l10n.t("invite.links.custom_uses_count"), text: $form.customUses)
                if form.showsCustomUsesError {
                    errorLine(l10n.t(
                        "invite.links.error.custom_uses",
                        texts: ["max": Int(InviteCreateForm.customMaxUses)
                            .formatted(.number.locale(l10n.locale))]))
                }
            }
        } header: {
            Text(l10n.t("invite.links.uses_field"))
        }
        .disabled(viewModel.generatingInvite)

        Section {
            HStack(spacing: 12) {
                Button {
                    guard let params = form.params else { return }
                    viewModel.onCreateInviteClicked(params) { creating = false }
                } label: {
                    HStack(spacing: 8) {
                        if viewModel.generatingInvite {
                            ProgressView().controlSize(.small)
                            Text(l10n.t("chats.new_group.creating"))
                        } else {
                            Text(l10n.t("invite.links.create"))
                        }
                    }
                    .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(Color.lakkoAccent)
                .disabled(viewModel.generatingInvite || form.params == nil)

                Button {
                    creating = false
                    viewModel.clearInviteError()
                } label: {
                    Text(l10n.t("common.cancel"))
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .tint(Color.lakkoAccent)
                .disabled(viewModel.generatingInvite)
            }
            .controlSize(.large)
            if let error = viewModel.inviteError {
                errorLine(error.text)
            }
        } footer: {
            Text(l10n.t("invite.links.desc"))
        }
    }

    /// The label as typed, never longer than the core accepts.
    private var labelBinding: Binding<String> {
        Binding(
            get: { form.label },
            set: { form.label = InviteCreateForm.clampLabel($0) })
    }

    private func numberField(title: String, text: Binding<String>) -> some View {
        HStack {
            Text(title)
                .foregroundStyle(Color.lakkoText)
            Spacer(minLength: 12)
            TextField(title, text: text)
                .keyboardType(.numberPad)
                .multilineTextAlignment(.trailing)
                .frame(maxWidth: 120)
                .accessibilityLabel(title)
        }
    }

    private func errorLine(_ text: String) -> some View {
        Text(text)
            .font(.footnote)
            .foregroundStyle(Color.lakkoDanger)
    }

    private func details(for row: InviteLinkRow) -> [String] {
        var lines: [String] = []
        if let created = row.createdAt {
            let date = created.formatted(
                Date.FormatStyle(date: .abbreviated, time: .shortened).locale(l10n.locale))
            lines.append(l10n.t("invite.links.created", texts: ["date": date]))
        }
        var status = InviteLinkRows.expiryCopy(expiryMs: row.expiryMs, now: now)
        status = l10n.t(
            "common.fact_pair", texts: ["first": status, "second": InviteLinkRows.useFact(row)])
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

/// The link a QR sheet shows.
private struct QrLink: Identifiable {
    let url: String
    var id: String { url }
}

/// A large QR code of one link, the hint who may scan it, and Done. The link
/// is never handed to the system share sheet.
struct LinkQrSheet: View {
    let url: String
    let onDismiss: () -> Void
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        NavigationStack {
            VStack(spacing: 20) {
                Spacer(minLength: 0)
                // A light quiet zone around the code so a camera reads it in
                // dark mode too.
                QrTextImageView(text: url, size: 260)
                    .padding(16)
                    .background(Color.white)
                    .clipShape(RoundedRectangle(cornerRadius: 16))
                    .accessibilityLabel(l10n.t("common.qr_code"))
                Text(l10n.t("invite.links.qr_hint"))
                    .font(.callout)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 24)
                Spacer(minLength: 0)
            }
            .frame(maxWidth: .infinity)
            .navigationTitle(l10n.t("common.qr_code"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button(l10n.t("common.done"), action: onDismiss)
                }
            }
        }
        .presentationDetents([.large])
    }
}

/// One shareable link as a row: title (when the link has a label), detail
/// lines, then Copy, Show QR code and (when offered) Revoke. A link always
/// looks the same and the URL itself stays out of the way.
struct LinkActionsRow: View {
    /// The link's label; `nil` when it has none — the facts then lead.
    let title: String?
    var details: [String] = []
    /// `nil` when the link's secret is not on this device (a link another
    /// device minted, ADR-0028 amendment 2026-09-28): Revoke only.
    let url: String?
    var highlighted: Bool = false
    var revoking: Bool = false
    var revoke: (() -> Void)? = nil
    /// Opens the link's QR code; `nil` when this device has no URL to show.
    var showQr: (() -> Void)? = nil

    @State private var copied = false
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if title != nil || highlighted {
                titleLine
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
                        Label(l10n.t(copied ? "common.copied" : "common.copy"), systemImage: copied ? "checkmark" : "doc.on.doc")
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    // A link is never handed to the system's share sheet:
                    // Copy, or a QR code someone scans.
                    if let showQr {
                        Button(action: showQr) {
                            Image(systemName: "qrcode")
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .accessibilityLabel(l10n.t("invite.links.show_qr"))
                    }
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

    private var titleLine: some View {
        HStack(spacing: 8) {
            if let title {
                Text(title)
                    .font(.body.weight(.medium))
                    .foregroundStyle(Color.lakkoText)
            }
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
    }
}
