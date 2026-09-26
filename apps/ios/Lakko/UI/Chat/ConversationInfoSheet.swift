// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
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
                    // the screen this layer sits over (section 2.1). Which
                    // rows that removes is the shared rule's answer, not a
                    // flag passed down.
                    onOpenChat: { _ in },
                    onCall: onCall,
                    presentation: .sheet,
                    origin: .conversation,
                    onDone: { chat.closeConversationInfo() },
                    // ADR-0045 UX 6.2.4 — an ended one-to-one used to have no
                    // exit at all: `Delete this chat` lived in the group panel,
                    // which a one-to-one stopped opening when this layer split
                    // by destination. The person layer carries it now, and the
                    // screen behind it has nothing left to show.
                    onChatDeleted: {
                        chat.closeConversationInfo()
                        onDeleted()
                    }
                )
            } else {
                UnknownPersonInfo(chat: chat, onCall: onCall, onDeleted: onDeleted)
            }
        case .group:
            MembersSheet(viewModel: chat, onDeleted: onDeleted)
        case .channel:
            // Unreachable from here: a channel is not a chat screen, so
            // `ConversationInfoDestination.of(kind:contactMessageId:)` never
            // returns this. A channel has its own info layer, opened by its
            // own name — ``ChannelInfoSheet`` — reached from
            // ``ChannelDetailScreen`` rather than from a chat.
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
    /// Start a call on this conversation. Not being a contact does not stop
    /// one: the per-call key comes from the GROUP, which exists, and the
    /// Android twin has always offered it here.
    let onCall: (_ groupIdHex: String, _ video: Bool) -> Void
    /// The chat is gone and the screen behind this has nothing left to show.
    let onDeleted: () -> Void

    @State private var confirmingDeleteChat = false

    /// The same shared rule the contact screen asks, with the one capability
    /// that differs: there is no contact record here, so nothing that acts on
    /// one is offered. What survives is `Call` on a live membership and
    /// `Delete this chat` on an ended one — this person is the narrower half
    /// of the defect where an ended one-to-one had no exit, and it would
    /// still have none if only the contact screen carried the control.
    ///
    /// Asking the rule caught a second thing on the way: this view offered no
    /// call at all while the Android twin did, for no reason either client
    /// could name. The rule says `.call`, so it is here now.
    private var sections: [InfoSection] {
        personInfoSections(
            origin: .conversation,
            caps: PersonInfoCapabilities(
                isContact: false, membershipEnded: chat.membershipHasEnded))
    }

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

                if sections.contains(.call) {
                    Section {
                        HStack(spacing: 12) {
                            Button {
                                onCall(chat.groupIdHex, false)
                            } label: {
                                Label("Call", systemImage: "phone.fill")
                                    .frame(maxWidth: .infinity)
                            }
                            Button {
                                onCall(chat.groupIdHex, true)
                            } label: {
                                Label("Video", systemImage: "video.fill")
                                    .frame(maxWidth: .infinity)
                            }
                        }
                        .buttonStyle(.borderless)
                        // The sheet's controls take the Lakko accent, not the
                        // platform's default blue (seen live 2026-09-26).
                        .tint(Color.lakkoAccent)
                        .listRowBackground(Color.clear)
                    }
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

                if sections.contains(.deleteChat) {
                    Section {
                        Button(role: .destructive) { confirmingDeleteChat = true } label: {
                            Text(deleteChatControl)
                        }
                    }
                }
            }
            .confirmationDialog(
                deleteChatTitle,
                isPresented: $confirmingDeleteChat,
                titleVisibility: .visible
            ) {
                Button("Delete", role: .destructive) {
                    if chat.onDeleteChat() {
                        chat.closeConversationInfo()
                        onDeleted()
                    }
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text(deleteChatBody)
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
