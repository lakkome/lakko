// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// `docs/ux/group-and-channel-privacy.md` section 5.1 — the card of a public
/// group or channel found by search: avatar, name, `@name`, kind and size,
/// description, and one button (`Open`, `Request to join` / `Request sent`,
/// or `Subscribe`). No member is listed and no message is shown.
struct ConversationCardScreen: View {
    let found: ConversationHandles.Found
    let repository: ChatRepository
    /// Whether this device already holds the group or follows the channel
    /// (a row in its conversation list).
    let isHeld: (String) -> Bool
    /// Opens the conversation when this device holds it; `false` when it does
    /// not.
    let openHeld: (String) -> Bool
    /// Subscribes to the channel and opens it under `name`.
    let subscribe: (_ channelIdHex: String, _ handle: String, _ name: String) -> Void
    let onClose: () -> Void

    @ObservedObject private var l10n = Localization.shared
    @State private var sending = false
    @State private var lineKey: String?
    @State private var sent = false

    private var held: Bool { isHeld(found.idHex) || repository.groups[found.idHex] != nil }

    private var alreadySent: Bool {
        sent || (AcceptedContactsStore.boundAccountIdB64.map {
            RequestsSentStore.isOpen(accountIdB64: $0, groupIdHex: found.idHex)
        } ?? false)
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 12) {
                    InitialsAvatar(
                        name: found.name, identity: found.idHex, size: 96, isGroup: true,
                        photo: found.avatar.map(AvatarPhoto.avatar), playsVideo: true)
                    Text(found.name).font(.title3.weight(.semibold)).foregroundStyle(Color.lakkoText)
                    Text("@" + found.handle)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .environment(\.layoutDirection, .leftToRight)
                    Text(ConversationFoundRow.line(found, l10n: l10n))
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                    if !found.description.isEmpty {
                        Text(found.description)
                            .foregroundStyle(Color.lakkoText)
                            .multilineTextAlignment(.center)
                            .padding(.top, 8)
                    }
                    button.padding(.top, 12)
                    if let lineKey {
                        Text(l10n.t(lineKey))
                            .font(.footnote)
                            .foregroundStyle(lineKey == "search.conversation.request_sent"
                                             ? Color.lakkoTextMuted : Color.lakkoDanger)
                            .multilineTextAlignment(.center)
                    }
                }
                .padding(24)
                .frame(maxWidth: .infinity)
            }
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button { onClose() } label: { LkIconView(.chevronBack, size: 24) }
                        .accessibilityLabel(l10n.t("common.back"))
                }
            }
        }
    }

    @ViewBuilder
    private var button: some View {
        if held {
            AuthPrimaryButton(title: l10n.t("search.conversation.open")) {
                if openHeld(found.idHex) { onClose() }
            }
        } else if found.kind == .channel {
            AuthPrimaryButton(title: l10n.t("search.conversation.subscribe")) {
                onClose()
                subscribe(found.idHex, found.handle, found.name)
            }
        } else if alreadySent {
            AuthPrimaryButton(title: l10n.t("search.conversation.request_sent_button"), disabled: true) {}
        } else {
            AuthPrimaryButton(title: l10n.t("search.conversation.request"), busy: sending) { request() }
        }
    }

    private func request() {
        sending = true
        lineKey = nil
        Task { @MainActor in
            defer { sending = false }
            let outcome = await repository.requestToJoinPublicGroup(
                groupId: found.id, knockKey: found.key, knockVersion: found.knockVersion)
            switch outcome {
            case .sent:
                sent = true
                lineKey = "search.conversation.request_sent"
                if let accountIdB64 = AcceptedContactsStore.boundAccountIdB64 {
                    RequestsSentStore.recordPublic(
                        groupId: found.id, name: found.name, handle: found.handle, knockKey: found.key,
                        knockVersion: found.knockVersion, accountIdB64: accountIdB64,
                        nowMs: Int64(Date().timeIntervalSince1970 * 1000))
                }
            case .queueFull: lineKey = "search.conversation.queue_full"
            case .outOfRequests: lineKey = "search.conversation.out_of_requests"
            case .offline, .failed: lineKey = "group_handle.error.offline"
            }
        }
    }
}
