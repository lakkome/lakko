// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The layer that opens over a conversation when its name is tapped
/// (`docs/ux/conversation-info.md` sections 1 and 3).
///
/// One entry point, and what is behind it follows from the conversation. The
/// decision itself is ``ConversationInfoDestination``, which a test can run;
/// this only renders what that decided.
struct ConversationInfoSheet: View {
    let destination: ConversationInfoDestination
    @ObservedObject var chat: ChatViewModel
    @ObservedObject var contacts: ContactsViewModel
    /// Start a call with this person, from the person's own screen.
    let onCall: (_ groupIdHex: String, _ video: Bool) -> Void
    /// The chat is gone and the screen behind this layer has nothing left to
    /// show (ADR-0045 UX section 6.2.4).
    let onDeleted: () -> Void

    var body: some View {
        switch destination {
        case .person(let contactMessageId):
            if let contactMessageId {
                ContactDetailScreen(
                    viewModel: contacts,
                    messageId: contactMessageId,
                    // Opened from the chat, so "Message" would lead back to
                    // the screen this layer sits over (section 2.1).
                    onOpenChat: { _ in },
                    onCall: onCall,
                    presentation: .sheet,
                    offersMessage: false,
                    onDone: { chat.closeConversationInfo() }
                )
            } else {
                UnknownPersonInfo(chat: chat)
            }
        case .group:
            MembersSheet(viewModel: chat, onDeleted: onDeleted)
        case .channel:
            // Unreachable from here: a channel is not a chat screen, so
            // `ConversationInfoDestination.of(kind:contactMessageId:)` never
            // returns this. A channel opens `ChannelDetailScreen`, which is
            // both its conversation and its info.
            EmptyView()
        }
    }
}

/// The person on the other side of a conversation this device holds no
/// contact for.
///
/// A real state rather than an error: a conversation can arrive before the
/// contact does. What is known about them is still shown — the same name this
/// device puts on their messages — and the one thing that is not known is
/// said plainly, instead of the contact screen's "no longer in your contacts",
/// which would be false.
private struct UnknownPersonInfo: View {
    @ObservedObject var chat: ChatViewModel

    var body: some View {
        NavigationStack {
            List {
                Section {
                    VStack(spacing: 8) {
                        if let key = chat.oneToOnePeerSignatureKey {
                            ResolvedName(signatureKey: key) { resolved in
                                InitialsAvatar(
                                    name: resolved.isPlaceholder ? "" : resolved.name,
                                    identity: base64Encode(key),
                                    size: 72,
                                    placeholderSymbol: "person.fill"
                                )
                                Text(resolved.name)
                                    .font(.title2.weight(.semibold))
                                    .foregroundStyle(Color.lakkoText)
                                    .multilineTextAlignment(.center)
                                if let line = resolved.usernameLine {
                                    Text(line)
                                        .font(.subheadline)
                                        .foregroundStyle(Color.lakkoTextMuted)
                                }
                            }
                        } else {
                            InitialsAvatar(
                                name: "",
                                identity: chat.groupIdHex,
                                size: 72,
                                placeholderSymbol: "person.fill"
                            )
                            Text(ResolvedDisplayName.placeholderMember)
                                .font(.title2.weight(.semibold))
                                .foregroundStyle(Color.lakkoText)
                        }
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 8)
                    .listRowBackground(Color.clear)
                }

                Section {
                    Text("Not one of your contacts.")
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                } footer: {
                    Text(
                        "Add them to your contacts to compare safety numbers and "
                            + "give them a name only you see."
                    )
                }
            }
            .listStyle(.insetGrouped)
            .navigationTitle("Contact")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { chat.closeConversationInfo() }
                }
            }
        }
    }
}
