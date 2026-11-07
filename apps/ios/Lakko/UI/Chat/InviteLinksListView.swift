// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import SwiftUI

/// One row of an invite-link list: what the screen shows and copies.
struct InviteLinkListRow: Identifiable, Equatable {
    /// The ledger id as an unpadded base64url slug.
    let id: String
    let url: String
    let label: String
}

/// `docs/ux/group-and-channel-privacy.md` section 14.2 — the invite-link
/// screen a group and a channel share: a new link with an optional label,
/// expiry and use limit, then the list of the account's live links (made on
/// any device of the account, through the synced link register) with
/// `Copy link` and `Revoke`. The caller supplies where links come from and
/// how they are made and revoked; the screen holds no rule of its own.
struct InviteLinksListView: View {
    let load: () -> [InviteLinkListRow]
    let create: (_ label: String, _ expiryMs: UInt64, _ maxUses: UInt32) async throws -> Void
    /// Revoke one link. Returns a notice key to show (for instance a link
    /// only cleared because no device holds its key), or `nil`.
    let revoke: (InviteLinkListRow) async throws -> String?
    let createErrorKey: (Error) -> String
    let revokeErrorKey: (Error) -> String

    @ObservedObject private var l10n = Localization.shared
    @State private var links: [InviteLinkListRow] = []
    @State private var form = ChannelLinkForm()
    @State private var busy = false
    @State private var errorKey: String?
    @State private var noticeKey: String?
    @State private var revoking: InviteLinkListRow?
    @State private var copied: String?

    var body: some View {
        List {
            Section {
                TextField(l10n.t("invite.links.label_placeholder"), text: $form.label)
                    .textInputAutocapitalization(.sentences)
                LakkoPicker(
                    l10n.t("invite.links.expires_field"), selection: $form.expiry,
                    options: [
                        LakkoPickerOption(value: .oneDay, label: l10n.t("invite.links.expires_option_days", counts: ["count": 1])),
                        LakkoPickerOption(value: .sevenDays, label: l10n.t("invite.links.expires_option_days", counts: ["count": 7])),
                        LakkoPickerOption(value: .thirtyDays, label: l10n.t("invite.links.expires_option_days", counts: ["count": 30])),
                        LakkoPickerOption(value: .never, label: l10n.t("invite.links.expires_option_never")),
                    ])
                LakkoPicker(
                    l10n.t("invite.links.uses_field"), selection: $form.uses,
                    options: [
                        LakkoPickerOption(value: .one, label: l10n.t("invite.links.uses_option_one")),
                        LakkoPickerOption(value: .upToTen, label: l10n.t("invite.links.uses_option_up_to", counts: ["count": 10])),
                        LakkoPickerOption(value: .unlimited, label: l10n.t("invite.links.uses_option_unlimited")),
                    ])
                Button(l10n.t("invite.links.create")) { Task { await createLink() } }
                    .disabled(busy)
                if let errorKey {
                    Text(l10n.t(errorKey)).font(.footnote).foregroundStyle(Color.lakkoDanger)
                }
                if let noticeKey {
                    Text(l10n.t(noticeKey)).font(.footnote).foregroundStyle(Color.lakkoTextMuted)
                }
            }
            if !links.isEmpty {
                Section {
                    ForEach(links) { link in
                        VStack(alignment: .leading, spacing: 6) {
                            if !link.label.isEmpty {
                                Text(link.label).font(.subheadline).foregroundStyle(Color.lakkoText)
                            }
                            Text(link.url)
                                .font(.footnote)
                                .foregroundStyle(Color.lakkoTextMuted)
                                .lineLimit(1)
                                .truncationMode(.middle)
                                .environment(\.layoutDirection, .leftToRight)
                            HStack {
                                Button(l10n.t(copied == link.id ? "common.copied" : "chat.invite_link.copy")) {
                                    UIPasteboard.general.string = link.url
                                    copied = link.id
                                }
                                .buttonStyle(.borderless)
                                Spacer()
                                Button(l10n.t("chat.invite_link.revoke"), role: .destructive) { revoking = link }
                                    .buttonStyle(.borderless)
                            }
                        }
                    }
                } header: {
                    Text(l10n.t("invite.links.list"))
                }
            }
        }
        .listStyle(.insetGrouped)
        .disabled(busy)
        .navigationTitle(l10n.t("chat.invite_link.label"))
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { links = load() }
        // A link made or revoked on another device of the account arrives in
        // the synced register; the list follows it.
        .onReceive(Self.registerChanges) { _ in links = load() }
        .lakkoDialog(item: $revoking) { link in
            LakkoDialogContent(
                title: l10n.t("chat.invite_link.revoke"), message: nil, field: nil,
                actions: [.danger(l10n.t("chat.invite_link.revoke")) { Task { await revokeLink(link) } }, .cancel()])
        }
    }

    private static var registerChanges: AnyPublisher<Void, Never> {
        guard let sync = GroupLinkRegister.sync else { return Empty().eraseToAnyPublisher() }
        return sync.$linkRecords.dropFirst().map { _ in () }.receive(on: RunLoop.main).eraseToAnyPublisher()
    }

    private func createLink() async {
        busy = true
        errorKey = nil
        noticeKey = nil
        defer { busy = false }
        do {
            let now = Int64(Date().timeIntervalSince1970 * 1000)
            try await create(
                form.label.trimmingCharacters(in: .whitespacesAndNewlines), form.expiryMs(nowMs: now), form.maxUses)
            links = load()
            form = ChannelLinkForm()
        } catch {
            errorKey = createErrorKey(error)
        }
    }

    private func revokeLink(_ link: InviteLinkListRow) async {
        errorKey = nil
        noticeKey = nil
        do {
            noticeKey = try await revoke(link)
            links = load()
        } catch {
            errorKey = revokeErrorKey(error)
        }
    }
}

/// Section 14.2 for a group: the group's links through the chat repository.
struct GroupInviteLinksScreen: View {
    @ObservedObject var viewModel: ChatViewModel

    var body: some View {
        InviteLinksListView(
            load: { viewModel.groupInviteLinks().map(Self.row) },
            create: { label, expiryMs, maxUses in
                try await viewModel.createGroupInviteLink(label: label, expiryMs: expiryMs, maxUses: maxUses)
            },
            revoke: { row in
                guard let link = viewModel.groupInviteLinks().first(where: { Self.row($0).id == row.id }) else {
                    return nil
                }
                let revoked = try await viewModel.revokeGroupInviteLink(link)
                return revoked ? nil : ChatViewModel.inviteLinkRevokeNotice(revoked: false)
            },
            createErrorKey: { error in
                error is URLError || DsTransientFailure.isNetwork(error)
                    ? "group_handle.error.offline" : "chat.error.create_link"
            },
            revokeErrorKey: { error in
                error is URLError || DsTransientFailure.isNetwork(error)
                    ? "group_handle.error.offline" : "chat.error.revoke_link"
            })
    }

    static func row(_ link: GroupInviteLinkRow) -> InviteLinkListRow {
        InviteLinkListRow(id: base64UrlEncodeNoPad(link.ledgerId), url: link.url, label: link.label)
    }
}
