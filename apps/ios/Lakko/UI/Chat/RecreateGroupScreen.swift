// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// ADR-0065 phase 2 — `Recreate group` (`docs/ux/identity-recovery.md`
/// section 16.5): the new-group flow of `new-group.md` pre-filled, pushed
/// inside the members panel's own stack.
///
/// Pre-filled with every current member except the old owner, each may be
/// unticked; the old identity's new keys are listed under the contact,
/// unticked and disabled with the two-line reason while `reset-unverified`
/// on this device, ticked and enabled once verified. The creating admin
/// becomes the new group's owner by the ordinary rule of creation; other
/// admins become plain members and the sheet says nothing about roles.
///
/// `Create and close the old group` is ONE action (``ChatRepository/recreateGroup``):
/// create, invite every ticked member through the ordinary add path, post the
/// group-closed notice into the old group naming the new one, open the new
/// group. A founding failure keeps the form; some invitations failing keeps
/// the sheet with the names and `Done`, the group existing behind it.
struct RecreateGroupScreen: View {
    @ObservedObject var viewModel: ChatViewModel
    /// Finished: close the panel over the new group (opening it if it is not
    /// already open behind).
    let onCreated: (_ groupIdHex: String) -> Void
    /// Created, some invitations failed: open the new group BEHIND the panel,
    /// which stays with the notice and `Done`.
    var onCreatedBehind: (_ groupIdHex: String) -> Void = { _ in }

    @State private var name: String = ""
    @State private var selected: Set<String> = []
    @State private var seeded = false
    @State private var busy = false
    @State private var errorMessage: UiText?
    /// Created, some invitations failed: the sheet stays with the notice and
    /// `Done`.
    @State private var createdWithFailures: (groupIdHex: String, notice: String)?
    @Environment(\.dismiss) private var dismiss
    @ObservedObject private var l10n = Localization.shared

    private static let maxNameLength = 64

    private var candidates: [ChatViewModel.RecreateCandidate] { viewModel.recreateCandidates() }

    private var picks: [ChatViewModel.RecreateCandidate] {
        candidates.filter { selected.contains($0.id) && $0.disabled == nil }
    }

    var body: some View {
        List {
            Section {
                TextField(l10n.t("chats.new_group.name_label"), text: $name)
                    .disabled(busy || createdWithFailures != nil)
                    .onChange(of: name) { _, value in
                        if value.count > Self.maxNameLength {
                            name = String(value.prefix(Self.maxNameLength))
                        }
                        if value.contains(where: \.isNewline) {
                            name = value.filter { !$0.isNewline }
                        }
                    }
            }
            Section {
                ForEach(candidates) { candidate in
                    memberRow(candidate)
                }
            } header: {
                Text(
                    l10n.t(
                        "group.recreate.members_heading",
                        texts: ["count": candidates.count.formatted(.number.locale(Localization.currentLocale))]))
            } footer: {
                Text(l10n.t("group.recreate.note"))
            }
            if let errorMessage {
                Section {
                    Text(errorMessage.text)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoDanger)
                }
            }
            if let created = createdWithFailures {
                Section {
                    Text(created.notice)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                    Button(l10n.t("group.recreate.done")) { onCreated(created.groupIdHex) }
                }
            } else {
                Section {
                    Button(l10n.t(busy ? "group.recreate.working" : "group.recreate.submit")) {
                        Task { await create() }
                    }
                    .disabled(busy || picks.isEmpty)
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(l10n.t("group.recreate.title"))
        .navigationBarTitleDisplayMode(.inline)
        // UX 16.5 — "Working: the sheet cannot be dismissed".
        .interactiveDismissDisabled(busy)
        .navigationBarBackButtonHidden(busy)
        .toolbar {
            if !busy, createdWithFailures == nil {
                ToolbarItem(placement: .topBarTrailing) {
                    Button(l10n.t("common.cancel")) { dismiss() }
                }
            }
        }
        .onAppear(perform: seed)
    }

    /// Pre-fill once: every enabled candidate ticked. A verified old owner is
    /// ticked by default (16.5); an unverified one is not and cannot be.
    private func seed() {
        guard !seeded else { return }
        seeded = true
        name = viewModel.groupName ?? ""
        selected = Set(candidates.filter { $0.disabled == nil }.map(\.id))
    }

    @ViewBuilder
    private func memberRow(_ candidate: ChatViewModel.RecreateCandidate) -> some View {
        let isOn = selected.contains(candidate.id) && candidate.disabled == nil
        Button {
            guard candidate.disabled == nil else { return }
            if isOn { selected.remove(candidate.id) } else { selected.insert(candidate.id) }
        } label: {
            HStack(spacing: 12) {
                InitialsAvatar(
                    name: candidate.name,
                    identity: candidate.signatureKey.base64EncodedString(),
                    size: 40)
                VStack(alignment: .leading, spacing: 2) {
                    Text(candidate.name)
                        .foregroundStyle(candidate.disabled == nil ? Color.lakkoText : Color.lakkoTextMuted)
                    switch candidate.disabled {
                    case .resetUnverified?:
                        Label(l10n.t("group.recreate.excluded_reset"), systemImage: "exclamationmark.shield")
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    case .noAccountId?:
                        // No catalog key says "cannot be invited from this
                        // device" (reported as missing); the closest existing
                        // line is used rather than a disabled row with no
                        // reason.
                        Text(l10n.t("chats.new_chat.not_available"))
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    case nil:
                        EmptyView()
                    }
                }
                Spacer()
                Image(systemName: isOn ? "checkmark.circle.fill" : "circle")
                    .foregroundStyle(isOn ? Color.lakkoAccent : Color.lakkoTextMuted)
                    .accessibilityHidden(true)
            }
        }
        .disabled(candidate.disabled != nil || busy || createdWithFailures != nil)
        .accessibilityAddTraits(isOn ? .isSelected : [])
    }

    private func create() async {
        if busy { return }
        let chosen = picks.compactMap { candidate -> (accountId: Data, signatureKey: Data)? in
            guard let accountId = candidate.accountId else { return nil }
            return (accountId, candidate.signatureKey)
        }
        guard !chosen.isEmpty else { return }
        busy = true
        defer { busy = false }
        errorMessage = nil
        do {
            let result = try await viewModel.recreateGroup(name: name, members: chosen)
            if result.failedNames.isEmpty {
                onCreated(result.groupIdHex)
            } else {
                createdWithFailures = (
                    result.groupIdHex,
                    Localization.text(
                        "chat.members.add.error.some_failed",
                        texts: ["names": Localization.joinList(result.failedNames)]))
                onCreatedBehind(result.groupIdHex)
            }
        } catch is ChatRepository.RecreateCloseNotSent {
            // The new group exists; the close notice did not go out. The form
            // and its button stay: pressing again re-posts the notice naming
            // the same group and founds nothing.
            errorMessage = .key("common.error.offline")
        } catch {
            NSLog("ADR-0065 recreate: founding failed: \(error)")
            errorMessage = ChatViewModel.errorText(for: error, fallbackKey: "chats.new_group.error")
        }
    }
}
