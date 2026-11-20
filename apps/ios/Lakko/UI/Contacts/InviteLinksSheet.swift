// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// Contacts > Invite links (`docs/ux/contact-invite-link.md` section 9): the
/// account's live links with Copy, QR code, Edit (only while nobody can hold
/// the link) and Delete, and **Create link**, which makes a link with the
/// default terms at once. The rows are the account's synced link-record
/// register (``ContactsRepository/listActiveInvites()``) with the links
/// generated in this session that the list does not show yet
/// (``InviteLinkRows/merge``); whether a row offers Edit and whether Delete
/// asks first are read from its synced record. A raw URL is never shown in a
/// text field — a row is a title, the facts and the actions.
struct InviteLinksSheet: View {
    @ObservedObject var viewModel: ContactsViewModel
    let onDismiss: () -> Void
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    /// Frozen per body so every row's "expires in" agrees.
    @State private var now = Date()
    /// The link whose QR code is on screen, when one is.
    @State private var qrLink: QrLink?
    /// The slug of the link whose terms the edit form shows, when it is open.
    @State private var editingSlug: String?
    /// What the edit form holds; prefilled each time it opens.
    @State private var form = InviteCreateForm()
    /// The slug of the link whose deletion waits for confirmation.
    @State private var confirmingDeleteSlug: String?

    private var rows: [InviteLinkRow] {
        InviteLinkRows.merge(
            active: viewModel.activeInvites,
            sessionGenerated: viewModel.repositoryState.outstandingInvites,
            now: now,
            accountIdB64: viewModel.inviteAccountIdB64)
            .filter { !viewModel.deletedLinkSlugs.contains($0.slug) }
    }

    var body: some View {
        NavigationStack {
            List {
                if editingSlug != nil {
                    editForm
                } else {
                    createSection
                    listSection
                }
            }
            .listStyle(.insetGrouped)
            .navigationTitle(l10n.t(editingSlug == nil ? "contacts.invite_links.title" : "invite.links.edit_title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button(l10n.t("common.done"), action: onDismiss)
                }
            }
        }
        .overlay {
            if let slug = confirmingDeleteSlug {
                LakkoConfirmDialog(
                    title: l10n.t("invite.links.delete_confirm_title"),
                    message: l10n.t("invite.links.delete_confirm_body"),
                    primaryTitle: l10n.t("common.delete"),
                    primaryIsDestructive: true,
                    secondaryTitle: l10n.t("common.cancel"),
                    onPrimary: {
                        confirmingDeleteSlug = nil
                        viewModel.onDeleteLinkClicked(slug: slug)
                    },
                    onSecondary: { confirmingDeleteSlug = nil })
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

    // MARK: - Create and list

    private var createSection: some View {
        Section {
            Button {
                now = Date()
                viewModel.onCreateLinkClicked()
            } label: {
                HStack(spacing: 8) {
                    if viewModel.generatingInvite {
                        ProgressView().controlSize(.small)
                    }
                    Label(l10n.t("invite.links.create"), icon: .linkPlus)
                        .foregroundStyle(Color.lakkoAccent)
                }
            }
            .disabled(viewModel.generatingInvite)
            if let error = viewModel.inviteError {
                errorLine(error.text)
            }
        } footer: {
            Text(l10n.t("invite.links.desc"))
        }
    }

    private var listSection: some View {
        Section {
            if let error = viewModel.activeInvitesError {
                errorLine(error.text)
            }
            if let notice = viewModel.inviteNotice {
                Text(notice.text)
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
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
                    linkRow(row)
                }
            }
        } header: {
            Text(l10n.t("invite.links.list"))
        }
    }

    private func linkRow(_ row: InviteLinkRow) -> some View {
        let record = viewModel.linkRecord(slug: row.slug)
        let busy = viewModel.revokingActiveInviteSlug != nil || viewModel.generatingInvite
        let edit: (() -> Void)? = LinkRecords.editable(record) && !busy
            ? { openEdit(slug: row.slug, record: record) } : nil
        return LinkActionsRow(
            title: row.title,
            details: details(for: row),
            url: row.url,
            highlighted: row.slug == viewModel.lastGeneratedInvite?.blobIdSlug,
            deleting: viewModel.revokingActiveInviteSlug == row.slug,
            beforeCopy: { await viewModel.onCopyLinkClicked(slug: row.slug) },
            edit: edit,
            delete: { delete(row.slug, record: record) },
            showQr: row.url.map { url in { qrLink = QrLink(url: url) } }
        )
        .disabled(
            viewModel.revokingActiveInviteSlug != nil
                && viewModel.revokingActiveInviteSlug != row.slug)
    }

    private func delete(_ slug: String, record: LinkRecord?) {
        if LinkRecords.deleteNeedsConfirmation(record) {
            confirmingDeleteSlug = slug
        } else {
            viewModel.onDeleteLinkClicked(slug: slug)
        }
    }

    private func openEdit(slug: String, record: LinkRecord?) {
        guard let record else { return }
        form = InviteCreateForm.prefilled(
            label: record.label, expiryMs: record.expiryMs, maxUses: record.maxUses,
            mintedAtMs: record.mintedAtMs)
        viewModel.clearInviteError()
        editingSlug = slug
    }

    private func closeEdit() {
        editingSlug = nil
        now = Date()
        viewModel.clearInviteError()
    }

    // MARK: - Edit form

    /// The link's terms (label, Expires, Uses: the web form's choices and
    /// bounds), the line that saving makes a new link, then Save and Cancel.
    /// Save stays disabled while a custom field does not hold a valid value,
    /// and once the link can no longer be edited (copied or used meanwhile,
    /// on any device).
    @ViewBuilder
    private var editForm: some View {
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
            Text(l10n.t("invite.links.edit_note"))
                .font(.footnote)
                .foregroundStyle(Color.lakkoTextMuted)
            HStack(spacing: 12) {
                Button {
                    guard let slug = editingSlug, let params = form.params else { return }
                    viewModel.onSaveLinkTermsClicked(slug: slug, params: params) { closeEdit() }
                } label: {
                    HStack(spacing: 8) {
                        if viewModel.generatingInvite {
                            ProgressView().controlSize(.small)
                        }
                        Text(l10n.t("common.save"))
                    }
                    .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(Color.lakkoAccent)
                .disabled(viewModel.generatingInvite || form.params == nil || !stillEditable)

                Button {
                    closeEdit()
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
        }
    }

    /// The link the form edits is still editable per its synced record.
    private var stillEditable: Bool {
        guard let slug = editingSlug else { return false }
        return LinkRecords.editable(viewModel.linkRecord(slug: slug))
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
                // The branded code draws its own white card and quiet zone,
                // so a camera reads it in dark mode too.
                QrTextImageView(text: url, size: 264)
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
/// lines, then Copy, Show QR code, Edit (when offered) and the trash button
/// Delete link. A link always looks the same and the URL itself stays out of
/// the way.
struct LinkActionsRow: View {
    /// The link's label; `nil` when it has none — the facts then lead.
    let title: String?
    var details: [String] = []
    /// `nil` when the link's secret is not on this device (a link another
    /// device minted, ADR-0028 amendment 2026-09-28): Delete only.
    let url: String?
    var highlighted: Bool = false
    var deleting: Bool = false
    /// Runs before the link reaches the clipboard (marks it copied in the
    /// account's synced state).
    var beforeCopy: (() async -> Void)? = nil
    /// Opens the link's terms; `nil` when the link can no longer be edited.
    var edit: (() -> Void)? = nil
    var delete: (() -> Void)? = nil
    /// Opens the link's QR code; `nil` when this device has no URL to show.
    var showQr: (() -> Void)? = nil

    @State private var copied = false
    @State private var copying = false
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
                        copy(url)
                    } label: {
                        Label(l10n.t(copied ? "common.copied" : "common.copy"), icon: copied ? .checkmark : .copy)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(copying)
                    // A link is never handed to the system's share sheet:
                    // Copy, or a QR code someone scans.
                    if let showQr {
                        Button(action: showQr) {
                            LkIconView(.qrcode, size: 20)
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

                Spacer()
                if let edit {
                    Button(action: edit) {
                        LkIconView(.pencil, size: 20)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .accessibilityLabel(l10n.t("invite.links.edit"))
                }
                if let delete {
                    Button(role: .destructive, action: delete) {
                        if deleting {
                            ProgressView().controlSize(.small)
                        } else {
                            LkIconView(.trash, size: 20)
                        }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(deleting)
                    .accessibilityLabel(l10n.t("invite.links.delete"))
                }
            }
        }
        .padding(.vertical, 4)
    }

    /// Mark the link copied first, then the clipboard.
    private func copy(_ url: String) {
        guard !copying else { return }
        copying = true
        Task { @MainActor in
            await beforeCopy?()
            UIPasteboard.general.string = url
            copying = false
            copied = true
            try? await Task.sleep(nanoseconds: 1_500_000_000)
            copied = false
        }
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
