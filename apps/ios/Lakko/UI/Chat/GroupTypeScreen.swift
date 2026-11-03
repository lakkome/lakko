// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// `docs/ux/group-and-channel-privacy.md` section 4.1 — `Group type`, the
/// owner's pushed screen: who can join (public / private) and the `@name`.
/// Saving asks first; a flag change that cannot land now is owed and retried
/// until it does ("Will apply when you're back online.").
struct GroupTypeScreen: View {
    @ObservedObject var viewModel: ChatViewModel
    let onClose: () -> Void

    @ObservedObject private var l10n = Localization.shared
    @State private var isPublic: Bool
    @State private var handle: String
    @State private var saving = false
    @State private var lineKey: String?
    @State private var confirm: Confirm?

    private let initialPublic: Bool
    private let initialHandle: String

    enum Confirm: Identifiable {
        case makePublic, makePrivate, rename
        var id: Int { hashValue }
    }

    init(viewModel: ChatViewModel, onClose: @escaping () -> Void) {
        self.viewModel = viewModel
        self.onClose = onClose
        let current = viewModel.groupHandle
        initialPublic = current?.isPublic ?? false
        // Section 4.4 — a lapsed name opens with the proposed one.
        initialHandle = current?.handle
            ?? suggestHandle(
                name: viewModel.groupName ?? "", kind: .group, private: !(current?.isPublic ?? false),
                random: UInt64.random(in: 0...UInt64.max))
        _isPublic = State(initialValue: initialPublic)
        _handle = State(initialValue: initialHandle)
    }

    private var needsName: Bool { isPublic && (viewModel.groupName ?? "").isEmpty }
    private var changed: Bool {
        isPublic != initialPublic || ConversationHandles.canonical(handle) != initialHandle
            || viewModel.groupHandle == nil
    }
    private var canSave: Bool {
        changed && !saving && !needsName && ConversationHandles.wellFormed(handle)
    }

    var body: some View {
        List {
            Section {
                LakkoPicker(
                    l10n.t("group_type.who_can_join"), systemImage: "person.2",
                    selection: $isPublic,
                    options: [
                        LakkoPickerOption(value: false, label: l10n.t("group_type.private")),
                        LakkoPickerOption(value: true, label: l10n.t("group_type.public")),
                    ])
                HStack(spacing: 2) {
                    Text("@").foregroundStyle(Color.lakkoTextMuted)
                    TextField(l10n.t("group_type.handle"), text: Binding(
                        get: { handle }, set: { handle = ConversationHandles.canonical($0) }))
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .environment(\.layoutDirection, .leftToRight)
                }
            } footer: {
                VStack(alignment: .leading, spacing: 4) {
                    if !handle.isEmpty && !ConversationHandles.wellFormed(handle) {
                        Text(l10n.t("group_handle.error.bad_form")).foregroundStyle(Color.lakkoDanger)
                    } else {
                        Text(l10n.t(isPublic ? "group_type.handle_public" : "group_type.handle_private"))
                    }
                    Text(l10n.t(isPublic ? "group_type.public_desc" : "group_type.private_desc"))
                    if needsName {
                        Text(l10n.t("group_type.needs_name")).foregroundStyle(Color.lakkoDanger)
                    }
                    if let lineKey {
                        Text(l10n.t(lineKey))
                            .foregroundStyle(lineKey == "group_photo.owed" ? Color.lakkoTextMuted : Color.lakkoDanger)
                    } else if viewModel.groupTypeOwed {
                        Text(l10n.t("group_photo.owed")).foregroundStyle(Color.lakkoTextMuted)
                    }
                }
            }
        }
        .navigationTitle(l10n.t("group_type.title"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button(l10n.t(saving ? "common.saving" : "common.save")) { askFirst() }
                    .disabled(!canSave)
            }
        }
        .lakkoDialog(item: $confirm) { kind in
            switch kind {
            case .makePublic:
                return LakkoDialogContent(
                    title: l10n.t("group_type.confirm.public.title"),
                    message: l10n.t("group_type.confirm.public.body"),
                    field: nil,
                    actions: [
                        .primary(l10n.t("group_type.confirm.public.confirm")) { save() },
                        .cancel(),
                    ])
            case .makePrivate:
                let waiting = viewModel.pendingJoins.count
                var body = l10n.t("group_type.confirm.private.body")
                if waiting > 0 {
                    body += " " + l10n.t("group_type.confirm.private.waiting", counts: ["count": UInt64(waiting)])
                }
                return LakkoDialogContent(
                    title: l10n.t("group_type.confirm.private.title"),
                    message: body,
                    field: nil,
                    actions: [
                        .primary(l10n.t("group_type.confirm.private.confirm")) { save() },
                        .cancel(),
                    ])
            case .rename:
                return LakkoDialogContent(
                    title: l10n.t("group_type.confirm.rename.title"),
                    message: l10n.t("group_type.confirm.rename.body"),
                    field: nil,
                    actions: [
                        .primary(l10n.t("group_type.confirm.rename.confirm")) { save() },
                        .cancel(),
                    ])
            }
        }
    }

    private func askFirst() {
        guard canSave else { return }
        if viewModel.groupHandle != nil, ConversationHandles.canonical(handle) != initialHandle {
            confirm = .rename
        } else if isPublic != initialPublic {
            confirm = isPublic ? .makePublic : .makePrivate
        } else {
            save()
        }
    }

    private func save() {
        saving = true
        lineKey = nil
        Task { @MainActor in
            defer { saving = false }
            lineKey = await viewModel.saveGroupType(handle: handle, isPublic: isPublic)
            if lineKey == nil { onClose() }
        }
    }
}
