// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// What opens when the name at the top of a conversation is tapped
/// (`docs/ux/conversation-info.md` sections 1 and 2).
///
/// The rules live here, as plain values, rather than inside a SwiftUI body:
/// no view in this target is reachable from a test, which is how one panel
/// came to do three jobs and a two-person chat came to offer `Leave group`.
/// Stating the destination and its contents as data makes both checkable.

/// Which info a conversation's name opens — section 1's table.
///
/// Decided by what the conversation IS, never by which control was tapped:
/// there is one way in, and the destination follows from the conversation.
public enum ConversationInfoDestination: Equatable {
    /// A conversation between two people opens the other person.
    ///
    /// - Parameter contactMessageId: the accepted contact behind them, keyed
    ///   on the request's mailbox id — the same key the contacts list uses.
    ///   `nil` when this device holds no contact for the other person, which
    ///   is a real state (a conversation that arrived before the contact did)
    ///   and not an error: what is known about them is still shown.
    case person(contactMessageId: Int64?)
    /// A group opens the group.
    case group
    /// A channel opens the channel.
    case channel(channelId: String)

    /// The destination for a conversation the chat screen shows — a chat
    /// between two people, or a group. A channel is never a chat screen; it
    /// reaches its own info by being opened.
    public static func of(
        kind: ConversationKind,
        contactMessageId: Int64?
    ) -> ConversationInfoDestination {
        switch kind {
        case .chat:
            return .person(contactMessageId: contactMessageId)
        case .group:
            return .group
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

/// What a conversation's info holds — section 2.
///
/// One flag per section the three destinations can carry between them. The
/// point is section 2.1's `Never` list: a conversation between two people has
/// no group to name, no owner to transfer, no admin to make, no link to mint
/// and nothing to leave, and this makes that a fact a test can assert rather
/// than a claim about a view body.
public struct ConversationInfoContent: Equatable {
    /// The shared name, editable by whoever may rename (ADR-0051).
    public let sharedName: Bool
    /// The roster, with add and remove for whoever may (ADR-0022).
    public let memberRoster: Bool
    /// Create, copy and revoke an invite link (ADR-0028).
    public let inviteLink: Bool
    /// Make admin and remove admin (ADR-0022).
    public let roles: Bool
    /// Transfer ownership (ADR-0045).
    public let ownership: Bool
    /// Leave the group (ADR-0045).
    public let leaveGroup: Bool
    /// The out-of-band check between two people.
    public let safetyNumber: Bool
    /// Stop following a channel.
    public let unsubscribe: Bool

    public static func of(_ destination: ConversationInfoDestination) -> ConversationInfoContent {
        switch destination {
        case .person:
            // Section 2.1. Everything group-shaped is false, and that is the
            // whole requirement rather than a consequence of one.
            return ConversationInfoContent(
                sharedName: false,
                memberRoster: false,
                inviteLink: false,
                roles: false,
                ownership: false,
                leaveGroup: false,
                safetyNumber: true,
                unsubscribe: false
            )
        case .group:
            // Section 2.2 — what exists today, minus its habit of appearing
            // where there is no group. The safety number is between two
            // people and belongs on each of them, not on the group.
            return ConversationInfoContent(
                sharedName: true,
                memberRoster: true,
                inviteLink: true,
                roles: true,
                ownership: true,
                leaveGroup: true,
                safetyNumber: false,
                unsubscribe: false
            )
        case .channel:
            // Section 2.3.
            return ConversationInfoContent(
                sharedName: true,
                memberRoster: false,
                inviteLink: false,
                roles: false,
                ownership: false,
                leaveGroup: false,
                safetyNumber: false,
                unsubscribe: true
            )
        }
    }

    /// Section 2.1's `Never` list, read as one question: does this carry a
    /// group name, an invite link, roles, ownership or `Leave group`?
    ///
    /// Only the person case is required to answer `false`. A channel answers
    /// `true` on its own name, which is why this is named for the list it
    /// checks and not for the shape of what it finds.
    public var carriesAnythingForbiddenForOneToOne: Bool {
        sharedName || inviteLink || roles || ownership || leaveGroup
    }
}
