// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// What opens when the name at the top of a conversation is tapped
/// (`docs/ux/conversation-info.md` sections 1 and 2).
///
/// **The rules are not here.** `infoTarget`, `personInfoSections`,
/// `groupInfoSections` and `channelInfoSections` live in
/// `crates/lakko-ui-rules`, shared with the web and Android clients over the
/// FFI. What this file owns is the ROUTING PAYLOAD that goes with a
/// destination — a contact's message id, a channel id — which the shared
/// `InfoTarget` deliberately says nothing about because no other client keys
/// its screens the same way.
///
/// This file used to hold the rules too, as a struct of booleans, while
/// Android held them as enum cases and web as an array of strings. The three
/// disagreed, and each disagreement was a feature one client simply never
/// built: `Remove contact` (missing here), `Unsubscribe` (missing here) and
/// `Delete this chat` on an ended one-to-one (missing here). One copy of a
/// rule cannot diverge from itself, so the copy moved and this asks it.

/// Which info a conversation's name opens — section 1's table.
///
/// Decided by what the conversation IS, never by which control was tapped:
/// there is one way in, and the destination follows from the conversation.
public enum ConversationInfoDestination: Equatable {
    /// A conversation between two people opens the other person.
    ///
    /// - Parameter contactMessageId: the accepted contact behind them, keyed
    ///   on the request's mailbox id — the same key the contacts list uses.
    ///   `nil` when no record can be resolved - a roster that cannot be read
    ///   - in which case the header opens nothing. Every one-to-one peer is
    ///   a contact by construction (section 2.1), so this is not a "not a
    ///   contact" state; there is no screen for it.
    case person(contactMessageId: Int64?)
    /// A group opens the group.
    case group
    /// A channel opens the channel.
    case channel(channelId: String)

    /// The destination for a conversation the chat screen shows — a chat
    /// between two people, or a group.
    ///
    /// The kind is the app's own (``ConversationKind`` in
    /// `ConversationTitle.swift`), which has no channel case: a channel is
    /// never a chat screen, and reaches its own info through
    /// ``ofChannel(channelId:)``.
    public static func of(
        kind: ConversationKind,
        contactMessageId: Int64?
    ) -> ConversationInfoDestination {
        carrying(
            infoTarget(kind: kind.sharedKind),
            contactMessageId: contactMessageId,
            channelId: "")
    }

    /// The destination a channel's name opens, from the channel's own screen.
    public static func ofChannel(channelId: String) -> ConversationInfoDestination {
        carrying(infoTarget(kind: .channel), contactMessageId: nil, channelId: channelId)
    }

    /// The one place a shared target is paired with the payload iOS routes
    /// on. Total over the three targets, so a fourth added to the shared rule
    /// fails to compile here rather than falling through to a default.
    private static func carrying(
        _ target: InfoTarget,
        contactMessageId: Int64?,
        channelId: String
    ) -> ConversationInfoDestination {
        switch target {
        case .person:
            return .person(contactMessageId: contactMessageId)
        case .group:
            return .group
        case .channel:
            return .channel(channelId: channelId)
        }
    }

    /// What the header's control says it will open, for a screen reader.
    public var accessibilityHint: String {
        switch self {
        case .person:
            return "Opens contact info"
        case .group:
            return "Opens group info"
        case .channel:
            return "Opens channel info"
        }
    }
}

extension ConversationKind {
    /// The shared kind this one names.
    ///
    /// The app's kind is chat-or-group because that is what a chat screen can
    /// be; the shared one adds `channel`, which no chat screen ever is. The
    /// mapping is one-way on purpose — nothing here turns a shared `.channel`
    /// into a local kind, because there is no local kind for it.
    var sharedKind: LakkoCore.ConversationKind {
        switch self {
        case .chat:
            return .chat
        case .group:
            return .group
        }
    }
}
