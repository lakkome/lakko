// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// `docs/ux/group-and-channel-privacy.md` section 4.1 — `Channel type`, the
/// owner's pushed screen: `Who can subscribe`, the `@name`, and while public
/// `Topic` and `Language`. There is no separate Discover control: public
/// means Lakko may suggest it. Saving asks first.
struct ChannelTypeScreen: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data
    let onClose: () -> Void

    @ObservedObject private var l10n = Localization.shared
    @State private var isPublic: Bool
    @State private var handle: String
    @State private var topic: UInt8
    @State private var language: String
    @State private var saving = false
    @State private var errorKey: String?
    @State private var confirm: Confirm?
    @State private var taken = false
    @State private var rateLimited = false
    @State private var takenCheck: Task<Void, Never>?

    private let initialPublic: Bool
    private let initialHandle: String
    private let initialTopic: UInt8
    private let initialLanguage: String
    private let hasName: Bool
    private let topics = channelTopics()

    enum Confirm: Identifiable {
        case makePublic, makePrivate, rename
        var id: Int { hashValue }
    }

    init(
        channelId: String, state: ChannelsUiState, client: ChannelsClient, accountIdentitySeed: Data,
        onClose: @escaping () -> Void
    ) {
        self.channelId = channelId
        self.state = state
        self.client = client
        self.accountIdentitySeed = accountIdentitySeed
        self.onClose = onClose
        let held = ChannelSettings.heldHandle(channelId: channelId)
        let meta = state.sharedMeta(channelId: channelId)
        let isPublic = state.extras(channelId).isPublic ?? held?.isPublic ?? false
        initialPublic = isPublic
        hasName = held != nil
        // Section 4.4 — a channel with no name opens with the proposed one.
        initialHandle = held?.handle ?? suggestHandle(
            name: state.channels.first { $0.id == channelId }?.title ?? "", kind: .channel,
            private: !isPublic, random: UInt64.random(in: 0...UInt64.max))
        initialTopic = meta?.topic ?? 0
        initialLanguage = (meta?.language).flatMap { $0.isEmpty ? nil : $0 } ?? DiscoverLanguages.appLanguage()
        _isPublic = State(initialValue: isPublic)
        _handle = State(initialValue: initialHandle)
        _topic = State(initialValue: initialTopic)
        _language = State(initialValue: initialLanguage)
    }

    private var changed: Bool {
        !hasName || isPublic != initialPublic || ConversationHandles.canonical(handle) != initialHandle
            || (isPublic && (topic != initialTopic || language != initialLanguage))
    }

    private var canSave: Bool {
        changed && !saving && ConversationHandles.wellFormed(handle) && !(isPublic && taken)
    }

    var body: some View {
        List {
            Section {
                LakkoPicker(
                    l10n.t("channel_type.who_can_subscribe"), systemImage: "person.2",
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
                if isPublic {
                    LakkoPicker(
                        l10n.t("discover.listing.topic"), selection: $topic,
                        options: topics.map { LakkoPickerOption(value: $0.id, label: DiscoverTopics.name(Int($0.id))) })
                    LakkoPicker(
                        l10n.t("discover.language"), selection: $language,
                        options: DiscoverLanguages.options(language).map {
                            LakkoPickerOption(value: $0, label: DiscoverLanguages.label($0))
                        })
                }
            } footer: {
                VStack(alignment: .leading, spacing: 4) {
                    if !handle.isEmpty && !ConversationHandles.wellFormed(handle) {
                        Text(l10n.t("group_handle.error.bad_form")).foregroundStyle(Color.lakkoDanger)
                    } else if isPublic && taken {
                        Text(l10n.t("group_handle.error.taken")).foregroundStyle(Color.lakkoDanger)
                    } else if isPublic && rateLimited {
                        Text(l10n.t("common.error.too_many_attempts")).foregroundStyle(Color.lakkoTextMuted)
                    } else {
                        Text(l10n.t(isPublic ? "channel_type.handle_public" : "channel_type.handle_private"))
                    }
                    Text(l10n.t(isPublic ? "channel_type.public_desc" : "channel_type.private_desc"))
                    if let errorKey {
                        Text(l10n.t(errorKey)).foregroundStyle(Color.lakkoDanger)
                    }
                }
            }
        }
        .onChange(of: handle) { _, _ in scheduleTakenCheck() }
        .onChange(of: isPublic) { _, _ in scheduleTakenCheck() }
        .navigationTitle(l10n.t("channel_type.title"))
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
                    title: l10n.t("channel_type.confirm.public.title"),
                    message: l10n.t("channel_type.confirm.public.body"), field: nil,
                    actions: [.primary(l10n.t("group_type.confirm.public.confirm")) { save() }, .cancel()])
            case .makePrivate:
                return LakkoDialogContent(
                    title: l10n.t("channel_type.confirm.private.title"),
                    message: l10n.t("channel_type.confirm.private.body"), field: nil,
                    actions: [.primary(l10n.t("group_type.confirm.private.confirm")) { save() }, .cancel()])
            case .rename:
                return LakkoDialogContent(
                    title: l10n.t("group_type.confirm.rename.title"),
                    message: l10n.t("channel_type.confirm.rename.body"), field: nil,
                    actions: [.primary(l10n.t("group_type.confirm.rename.confirm")) { save() }, .cancel()])
            }
        }
    }

    private func scheduleTakenCheck() {
        takenCheck?.cancel()
        taken = false
        rateLimited = false
        let typed = ConversationHandles.canonical(handle)
        guard isPublic, typed.count >= 3, ConversationHandles.wellFormed(typed), typed != initialHandle else { return }
        if let held = HandleTakenCheck.held(typed) {
            taken = held == .taken
            return
        }
        takenCheck = Task { @MainActor in
            try? await Task.sleep(nanoseconds: HandleTakenCheck.debounceNanoseconds)
            guard !Task.isCancelled, typed == ConversationHandles.canonical(handle) else { return }
            let answer = await HandleTakenCheck.check(typed)
            guard !Task.isCancelled, typed == ConversationHandles.canonical(handle) else { return }
            taken = answer == .taken
            rateLimited = answer == .rateLimited
        }
    }

    private func askFirst() {
        guard canSave else { return }
        if hasName, ConversationHandles.canonical(handle) != initialHandle {
            confirm = .rename
        } else if isPublic != initialPublic {
            confirm = isPublic ? .makePublic : .makePrivate
        } else {
            save()
        }
    }

    private func save() {
        saving = true
        errorKey = nil
        let change = ChannelSettings.TypeChange(
            isPublic: isPublic, handle: handle, topic: isPublic ? topic : 0, language: isPublic ? language : "")
        Task { @MainActor in
            defer { saving = false }
            do {
                try await ChannelSettings.saveType(
                    channelId: channelId, state: state, client: client, currentSeed: accountIdentitySeed,
                    change: change)
                onClose()
            } catch {
                errorKey = ChannelSettings.errorKey(error)
            }
        }
    }
}
