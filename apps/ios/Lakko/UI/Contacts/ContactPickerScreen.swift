// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The picker behind Chats > New chat and Groups > New group: the accepted
/// contacts, one row each, tapping opens that contact's conversation through
/// the same path the Contacts screen's "Open" uses.
///
/// A contact's chat is the one-to-one with them (``ContactConversation``),
/// never the id stored on the row by itself; a contact with an account and
/// no chat here gets a new one-to-one when picked, as Message on the card
/// does (2026-11-05). A contact with no known account cannot be picked; the
/// row says so instead of hiding it.
struct ContactPickerScreen: View {
    @ObservedObject var viewModel: ContactsViewModel
    let title: String
    var footer: String? = nil
    /// Called with the picked contact's chat.
    let onPick: (String) -> Void
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        List {
            if let footer {
                Section {
                    Text(footer)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
            }
            if viewModel.repositoryState.accepted.isEmpty {
                Section {
                    Text(l10n.t("chats.new_chat.no_contacts"))
                        .foregroundStyle(Color.lakkoTextMuted)
                }
            } else {
                Section {
                    ForEach(viewModel.repositoryState.accepted, id: \.request.fromPubkey) { entry in
                        ResolvedName(signatureKey: entry.request.fromPubkey) { resolved in
                            let chat = viewModel.chatGroupIdHex(for: entry)
                            let pickable = chat != nil || entry.request.fromAccountId != nil
                            let rowId = entry.request.messageId
                            Button {
                                if let chat {
                                    onPick(chat)
                                } else {
                                    Task {
                                        if let started = await viewModel.openOrStartChat(for: entry) {
                                            onPick(started)
                                        }
                                    }
                                }
                            } label: {
                                HStack(spacing: 12) {
                                    InitialsAvatar(
                                        name: resolved.name,
                                        identity: entry.request.fromPubkey.base64EncodedString(),
                                        size: 40,
                                        photo: .person(entry.request.fromPubkey)
                                    )
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(resolved.listLabel)
                                            .foregroundStyle(Color.lakkoText)
                                        if !pickable {
                                            Text(l10n.t("chats.new_chat.not_available"))
                                                .font(.footnote)
                                                .foregroundStyle(Color.lakkoTextMuted)
                                        } else if let line = ContactDetailScreen.messageLineKey(
                                            hasChat: chat != nil,
                                            waitingRequest: viewModel.hasWaitingRequest(for: entry),
                                            notice: viewModel.messageNotice[rowId]) {
                                            Text(l10n.t(line.key, texts: ["name": resolved.name]))
                                                .font(.footnote)
                                                .foregroundStyle(
                                                    line.danger ? Color.lakkoDanger : Color.lakkoTextMuted)
                                        } else if viewModel.startChatFailed.contains(rowId) {
                                            Text(l10n.t("contacts.error.start_chat"))
                                                .font(.footnote)
                                                .foregroundStyle(Color.lakkoDanger)
                                        }
                                    }
                                    Spacer(minLength: 0)
                                    if viewModel.startingChat.contains(rowId) {
                                        ProgressView()
                                    }
                                }
                            }
                            .disabled(!pickable || viewModel.startingChat.contains(rowId))
                        }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
    }
}
