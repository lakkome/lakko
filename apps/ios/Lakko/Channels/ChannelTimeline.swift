// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0087 section 13 — what the chat envelope inside one post says. The
/// plaintext of a post is the chat envelope: text with attachments, or the
/// ADR-0023 edit and delete ops whose target is a post's server cursor.
public enum ChannelPostBody: Equatable {
    case chat(text: String, attachments: [AttachmentRef], forward: ForwardRef?)
    case edit(target: UInt64, text: String)
    case delete(target: UInt64)
    /// Anything else a post may carry that a channel does not show (a
    /// reaction or a typing op from a hostile publisher, a service envelope).
    case ignored

    /// Read the envelope bytes a ``ChannelMetaRecord/classify(_:)`` `.text`
    /// carried. A plaintext that is not JSON reads as plain text (a post
    /// written before section 13), through the core's legacy fallback.
    public static func read(_ envelope: String) -> ChannelPostBody {
        let parsed = parseBodyEnvelope(bytes: Data(envelope.utf8))
        switch parsed.kind {
        case .chat:
            let forward: ForwardRef? = parsed.forwardedFrom == nil && parsed.forwardOriginMessageId == nil
                ? nil
                : ForwardRef(
                    author: parsed.forwardedFrom, originGroupIdHex: nil, originMessageId: nil)
            return .chat(text: parsed.text, attachments: parsed.attachments, forward: forward)
        case .edit:
            guard let target = parsed.target, target > 0 else { return .ignored }
            return .edit(target: UInt64(target), text: parsed.text)
        case .delete:
            guard let target = parsed.target, target > 0 else { return .ignored }
            return .delete(target: UInt64(target))
        default:
            return .ignored
        }
    }

    /// The feed's line for a post (ADR-0063 section 3): the text, else a word
    /// for what it carries. `nil` for what is not a post of its own.
    public var feedText: String? {
        switch self {
        case .chat(let text, let attachments, _):
            if !text.isEmpty { return text }
            guard let first = attachments.first else { return nil }
            return ChannelTimeline.attachmentWord(first)
        default:
            return nil
        }
    }
}

/// One decoded post, before the ops of later posts are applied to it.
public struct ChannelDecodedPost: Equatable {
    public let cursor: UInt64
    public let serverTsMs: UInt64
    public let epoch: UInt32
    public let state: State

    public enum State: Equatable {
        /// Opened; what it says.
        case body(ChannelPostBody)
        /// A metadata or ban-list record, or a dropped malformed one: never a
        /// timeline row.
        case hidden
        /// No seed for its epoch on this device (section 13: "Earlier posts
        /// aren't available.").
        case noKey
        /// A seed is held and the post did not open under it.
        case unreadable
    }

    public init(cursor: UInt64, serverTsMs: UInt64, epoch: UInt32, state: State) {
        self.cursor = cursor
        self.serverTsMs = serverTsMs
        self.epoch = epoch
        self.state = state
    }
}

/// One row of the channel timeline.
public struct ChannelTimelineItem: Equatable, Identifiable {
    public let cursor: UInt64
    public let serverTsMs: UInt64
    public let epoch: UInt32
    public var text: String
    public let attachments: [AttachmentRef]
    public let forward: ForwardRef?
    public var edited: Bool
    /// The ADR-0023 op that changed it last (its cursor), for the `edited`
    /// mark's ordering.
    public var lastOpCursor: UInt64?

    public var id: UInt64 { cursor }
}

/// The pure fold of a channel's posts into its timeline (section 13): edits
/// and deletes apply to the post their target names, in cursor order, the
/// highest op winning; a deleted post leaves the timeline for everyone.
public enum ChannelTimeline {

    public struct Folded: Equatable {
        public var items: [ChannelTimelineItem]
        /// Some post on the server could not be opened on this device for
        /// want of its epoch's seed: the line "Earlier posts aren't
        /// available." shows at the top.
        public var earlierUnavailable: Bool
    }

    public static func fold(_ posts: [ChannelDecodedPost]) -> Folded {
        let ordered = posts.sorted { $0.cursor < $1.cursor }
        var items: [UInt64: ChannelTimelineItem] = [:]
        var order: [UInt64] = []
        var deleted: Set<UInt64> = []
        var earlier = false
        for post in ordered {
            switch post.state {
            case .noKey:
                earlier = true
            case .unreadable, .hidden:
                continue
            case .body(let body):
                switch body {
                case .chat(let text, let attachments, let forward):
                    items[post.cursor] = ChannelTimelineItem(
                        cursor: post.cursor, serverTsMs: post.serverTsMs, epoch: post.epoch,
                        text: text, attachments: attachments, forward: forward,
                        edited: false, lastOpCursor: nil)
                    order.append(post.cursor)
                case .edit(let target, let text):
                    // An op names an earlier post; one that names a later
                    // cursor or a post never seen does nothing.
                    guard target < post.cursor, var item = items[target], !deleted.contains(target) else { continue }
                    if let last = item.lastOpCursor, last > post.cursor { continue }
                    item.text = text
                    item.edited = true
                    item.lastOpCursor = post.cursor
                    items[target] = item
                case .delete(let target):
                    guard target < post.cursor else { continue }
                    deleted.insert(target)
                case .ignored:
                    continue
                }
            }
        }
        let visible = order.filter { !deleted.contains($0) }.compactMap { items[$0] }
        return Folded(items: visible, earlierUnavailable: earlier)
    }

    /// The word the feed and a row preview use for a post with no text.
    public static func attachmentWord(_ ref: AttachmentRef) -> String {
        switch InlineMedia.decision(for: ref).bubble {
        case .photo: return Localization.text("chat.media.photo")
        case .video: return Localization.text("chat.media.video")
        case .voice: return Localization.text("chat.media.voice_message")
        case .videoNote: return Localization.text("chat.media.video_message")
        case .gif: return Localization.text("chat.media.gif")
        case .file: return ref.name ?? Localization.text("chat.media.file")
        }
    }

    /// The timeline row of an item, in the shape the chat's message row
    /// draws: an incoming bubble with no author (section 12: the channel is
    /// the sender, on every device, the owner's included).
    public static func message(_ item: ChannelTimelineItem) -> ChatMessage {
        ChatMessage(
            messageId: Int64(clamping: item.cursor),
            direction: .incoming,
            text: item.text,
            attachments: item.attachments,
            createdAt: ChatRepository.isoTimestamp(ms: Int64(clamping: item.serverTsMs)),
            authorKeyB64: nil,
            edited: item.edited,
            deleted: false,
            forward: item.forward)
    }
}
