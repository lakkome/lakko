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
/// The layer a group's name opens (`docs/ux/conversation-info.md` section 3).
///
/// A person no longer has a layer: the chat header pushes the one contact
/// card, and a one-to-one whose record cannot be resolved opens nothing. The
/// "not one of your contacts" screen that used to live here, with its own
/// copy and its own Call row, is gone with the state it described
/// (2026-09-26).
struct ConversationInfoSheet: View {
    let destination: ConversationInfoDestination
    @ObservedObject var chat: ChatViewModel
    /// The chat is gone and the screen behind this layer has nothing left to
    /// show (ADR-0045 UX section 6.2.4).
    let onDeleted: () -> Void
    /// ADR-0065 phase 2 (UX 16.5) — the group was recreated: open the new one.
    var onRecreated: (_ groupIdHex: String) -> Void = { _ in }

    var body: some View {
        switch destination {
        case .group:
            MembersSheet(viewModel: chat, onDeleted: onDeleted, onRecreated: onRecreated)
        case .person, .channel:
            // Unreachable: the header pushes a person's card and never opens
            // this layer for one, and a channel is not a chat screen.
            EmptyView()
        }
    }
}
