// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// One list for every conversation: one-to-one chats, groups and channels.
///
/// `docs/ux/navigation.md` section 2. Chats, Groups and Channels used to be
/// three destinations for one activity — a person does not think "I will go to
/// the Groups place", they think of a conversation and look for it — and a list
/// sorted by WHEN cannot be three lists.
///
/// **The order itself is not here.** It is `order_conversations` in
/// `crates/lakko-ui-rules`, shared with the web and Android clients over the
/// FFI; what this file owns is the mapping between the rich row iOS draws and
/// the six fields that order reads. Chats and groups were already one store
/// split by a filter, so they merge by dropping it; channels are their own
/// store and have to be mapped in, which is the real work.
///
/// Pure of SwiftUI on purpose: a comparator written inside a `View` cannot be
/// run by a test, and this is the rule the whole list rests on.

/// Which screen a row opens.
///
/// The shared enum itself rather than a third local copy of it. Note that the
/// app also declares a `ConversationKind` of its own (`ConversationTitle.swift`,
/// chat-or-group, read from the group, ADR-0062); inside this module the
/// local one wins name resolution, so the shared one is always spelled out.
public typealias ConversationListKind = LakkoCore.ConversationKind

/// Section 11 — what the row's thumbnail draws: the first photo or video's
/// sent thumb, whether it is a video (a small play glyph), and how many
/// photos and videos the message holds (one stacked edge for two, two for
/// three or more).
public struct ConversationPreviewMedia: Equatable {
    public let thumb: ThumbRef?
    public let isVideo: Bool
    public let count: Int
    /// ADR-0047 section 6 — the micro thumbnail the message carries inside
    /// its envelope. Drawn first, with no fetch; ``thumb`` is fetched only
    /// when the message has none.
    public let inlineThumb: Data?

    public init(thumb: ThumbRef?, isVideo: Bool, count: Int, inlineThumb: Data? = nil) {
        self.thumb = thumb
        self.isVideo = isVideo
        self.count = count
        self.inlineThumb = inlineThumb
    }

    /// What the row draws, in order of preference: the inline bytes, else
    /// the sent thumb to fetch.
    public enum Source: Equatable {
        case inline(Data)
        case fetch(ThumbRef)
        case none
    }

    public var source: Source {
        if let inlineThumb { return .inline(inlineThumb) }
        if let thumb { return .fetch(thumb) }
        return .none
    }

    /// Section 11 — the offset edges behind the thumbnail.
    public var stackedEdges: Int { min(2, max(0, count - 1)) }
}

/// One row of the merged list, as iOS draws it.
///
/// **The row says nothing about which kind it is** (spec section 2.1): `kind`
/// is here to route a tap and for nothing else — no badge, no label, no
/// differing avatar. A person who subscribed to a channel knows they
/// subscribed to a channel.
public struct ConversationListRow: Identifiable, Equatable {
    /// Stable within a kind; the pair `(kind, id)` is unique in the list. A
    /// group id hex for a chat or a group, a channel id hex for a channel.
    public let id: String
    public let kind: ConversationListKind
    public let title: String
    /// The last message or post, already flattened to one line.
    public let preview: String
    /// When the last thing a person can SEE happened, in unix milliseconds: the
    /// last message, else the moment this device created or joined the
    /// conversation (``GroupArrival``). `nil` when neither is known.
    public let lastActivityMs: Int64?
    public let unread: Int
    public let pinned: Bool
    public let muted: Bool
    public let archived: Bool
    /// ADR-0045 UX §6.2.4 — the line that replaces the preview once this
    /// device's membership has ended. Always `nil` for a channel, which has no
    /// membership to end.
    public let endedLine: String?
    /// A manual "mark as unread" (the synced `unread_override` flag). Read by
    /// a folder's Read exclude toggle, which means what the badge means.
    public let markedUnread: Bool
    /// ADR-0084 — the other person's signature key for a one-to-one, so the
    /// row's circle can show their profile photo. `nil` for a group or a
    /// channel (their avatars are not profile photos).
    public let personKey: Data?

    /// What an avatar's tint is derived from: the person for a one-to-one
    /// (the same input every person screen uses, so one person has one
    /// colour everywhere), else the conversation.
    public var tintIdentity: String { personKey?.base64EncodedString() ?? id }
    /// ADR-0087 section 18 — the group whose photo the row's circle shows
    /// (`nil` for a one-to-one or a channel).
    public let photoGroupIdHex: String?
    /// Reactions-and-emoji spec section 12 — others reacted to this person's
    /// messages and those messages have not been on screen since.
    public let hasUnseenReactions: Bool
    /// `docs/ux/media-messages.md` section 11 — the first photo or video of
    /// the last message, drawn as a 20 pt thumbnail (a stack for several) at
    /// the start of the second line. `nil` when it carries none.
    public let previewMedia: ConversationPreviewMedia?

    public init(
        id: String,
        kind: ConversationListKind,
        title: String,
        preview: String,
        lastActivityMs: Int64?,
        unread: Int = 0,
        pinned: Bool = false,
        muted: Bool = false,
        archived: Bool = false,
        endedLine: String? = nil,
        markedUnread: Bool = false,
        personKey: Data? = nil,
        photoGroupIdHex: String? = nil,
        hasUnseenReactions: Bool = false,
        previewMedia: ConversationPreviewMedia? = nil
    ) {
        self.previewMedia = previewMedia
        self.personKey = personKey
        self.photoGroupIdHex = photoGroupIdHex
        self.hasUnseenReactions = hasUnseenReactions
        self.id = id
        self.kind = kind
        self.title = title
        self.preview = preview
        self.lastActivityMs = lastActivityMs
        self.unread = unread
        self.pinned = pinned
        self.muted = muted
        self.archived = archived
        self.endedLine = endedLine
        self.markedUnread = markedUnread
    }

    /// The row's second line as it is drawn: the ended line takes the preview's
    /// slot when there is one.
    public var secondLine: String { endedLine ?? preview }

    /// A conversation whose membership ended goes quiet: no unread badge, and
    /// no count in the spoken label either.
    public var showsUnread: Bool { unread > 0 && endedLine == nil }
}

/// The merged list, ordered and filtered.
public enum ConversationList {

    /// Pinned first, archived last, newest between — decided by the shared
    /// rule.
    ///
    /// `pinOrder` is the account's order of the pins as row ids, top first
    /// (the synced `ConversationOrder` register, navigation 2.2.1): it places
    /// the pinned rows it names; a pin it does not name sits above them,
    /// newest first. Every list built on this order passes it, so the lists
    /// agree.
    ///
    /// The rule hands back the input INDICES rather than rows, so the avatar,
    /// the preview and the unread count never cross the FFI boundary at all:
    /// this reorders the array it was already given.
    public static func sorted(
        _ rows: [ConversationListRow], pinOrder: [String] = []
    ) -> [ConversationListRow] {
        let order = LakkoCore.orderConversations(
            rows: rows.map(ChatFolders.ffiRow),
            pinOrder: PinOrder.spelledAsRows(pinOrder, rows: rows))
        return order.compactMap { index in
            let i = Int(index)
            return i < rows.count ? rows[i] : nil
        }
    }

    /// The local half of search (spec section 4): fuzzy over the titles and
    /// previews of what this device already holds, because that runs here.
    /// Anyone the person does not yet know needs the whole name and the
    /// network — see ``NameLookup``.
    public static func filtered(
        _ rows: [ConversationListRow], query: String
    ) -> [ConversationListRow] {
        rows.filter {
            ChatListFormatting.matches(query: query, title: $0.title, preview: $0.secondLine)
        }
    }
}
