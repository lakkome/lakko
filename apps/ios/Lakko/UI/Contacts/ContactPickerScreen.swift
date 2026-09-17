// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The picker behind Chats > New chat and Groups > New group: the accepted
/// contacts, one row each, tapping opens that contact's conversation through
/// the same path the Contacts screen's "Open" uses.
///
/// A contact whose chat was never created (the accept-time handshake failed)
/// is listed but cannot be picked; the row says so instead of hiding it.
struct ContactPickerScreen: View {
    @ObservedObject var viewModel: ContactsViewModel
    let title: String
    var footer: String? = nil
    /// Called with the picked contact's chat.
    let onPick: (String) -> Void

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
                    Text("No contacts yet. Add someone from the Contacts tab first.")
                        .foregroundStyle(Color.lakkoTextMuted)
                }
            } else {
                Section {
                    ForEach(viewModel.repositoryState.accepted, id: \.request.fromPubkey) { entry in
                        ResolvedName(signatureKey: entry.request.fromPubkey) { resolved in
                            Button {
                                if let groupIdHex = entry.groupIdHex { onPick(groupIdHex) }
                            } label: {
                                HStack(spacing: 12) {
                                    InitialsAvatar(
                                        name: resolved.name,
                                        identity: entry.request.fromPubkey.base64EncodedString(),
                                        size: 40
                                    )
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(resolved.listLabel)
                                            .foregroundStyle(Color.lakkoText)
                                        if entry.groupIdHex == nil {
                                            Text("Chat not set up yet")
                                                .font(.footnote)
                                                .foregroundStyle(Color.lakkoTextMuted)
                                        }
                                    }
                                }
                            }
                            .disabled(entry.groupIdHex == nil)
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
