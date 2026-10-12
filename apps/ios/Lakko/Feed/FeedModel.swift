// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The feed's input, assembled from what this device already holds
/// (ADR-0063 section 1, `docs/ux/feed.md` section 9). Mirror of web
/// `feed/feed-model.ts`.
///
/// The ranking is not here: it is `lakko-feed` in Rust, reached through
/// ``FeedRanking``, so the three clients order the same messages the same
/// way. This file only turns the per-account message log and the channel-post
/// log into the flat conversation and candidate lists the ranking reads, and
/// keeps a reference from each candidate back to the row it came from so the
/// screen can draw it.
///
/// One-to-one chats never reach it: the caller passes groups and channels
/// only, and the ranking's conversation kind has no value for a one-to-one
/// (UX spec section 3).

/// One group or channel the feed may take items from.
struct FeedSource: Equatable {
    enum Kind: Equatable {
        case group
        case channel
    }

    let kind: Kind
    /// The group's lowercase hex id, or the channel's 32-char hex id. Also
    /// the route that opens it.
    let idHex: String
    /// What the conversation list calls it.
    let title: String
    /// Muted in this account's conversation flags.
    let muted: Bool
    /// People in a group, for weighing reactions against its size; 0 for a
    /// channel, whose posts carry no reactions.
    let audience: Int
    /// ADR-0065 R.5 — the conversation a GROUP source belongs to (the
    /// lineage root), when it differs from nothing: `nil` for a channel.
    var conversationHex: String? = nil

    /// The key of everything a PERSON attaches to a feed source (the mute
    /// flag, "hide from feeds", a rule's include / exclude lists): the
    /// conversation id, never the group a moved conversation currently is.
    var key: String { LineageViews.feedSourceKey(idHex: idHex, conversationHex: conversationHex) }
}

/// A message reduced to what the feed draws and ranks. Built from a
/// ``ChatMessage`` for a group and from a ``PersistedChannelPost`` for a
/// channel, so both kinds of item are drawn one way.
struct FeedMessage: Equatable {
    enum Direction: Equatable {
        case outgoing
        case incoming
        case system
    }

    let messageId: Int64
    let direction: Direction
    let text: String
    /// Server time, unix milliseconds; `nil` when it does not parse.
    let timeMs: Int64?
    /// The author's signature key, base64, when known.
    let authorKeyB64: String?
    /// One `(reactor, emoji)` pair per reaction, in arrival order.
    let reactions: [(reactor: String, emoji: String)]
    /// The DS id this message replies to.
    let replyTarget: Int64?
    /// One label per attachment ("Photo", "Voice message", ...).
    let attachmentLabels: [String]
    let edited: Bool
    let deleted: Bool
    /// The original author's name when forwarded.
    let forwardAuthor: String?

    init(
        messageId: Int64,
        direction: Direction,
        text: String,
        timeMs: Int64?,
        authorKeyB64: String? = nil,
        reactions: [(reactor: String, emoji: String)] = [],
        replyTarget: Int64? = nil,
        attachmentLabels: [String] = [],
        edited: Bool = false,
        deleted: Bool = false,
        forwardAuthor: String? = nil
    ) {
        self.messageId = messageId
        self.direction = direction
        self.text = text
        self.timeMs = timeMs
        self.authorKeyB64 = authorKeyB64
        self.reactions = reactions
        self.replyTarget = replyTarget
        self.attachmentLabels = attachmentLabels
        self.edited = edited
        self.deleted = deleted
        self.forwardAuthor = forwardAuthor
    }

    static func == (lhs: FeedMessage, rhs: FeedMessage) -> Bool {
        lhs.messageId == rhs.messageId && lhs.direction == rhs.direction
            && lhs.text == rhs.text && lhs.timeMs == rhs.timeMs
            && lhs.authorKeyB64 == rhs.authorKeyB64
            && lhs.reactions.map(\.reactor) == rhs.reactions.map(\.reactor)
            && lhs.reactions.map(\.emoji) == rhs.reactions.map(\.emoji)
            && lhs.replyTarget == rhs.replyTarget
            && lhs.attachmentLabels == rhs.attachmentLabels
            && lhs.edited == rhs.edited && lhs.deleted == rhs.deleted
            && lhs.forwardAuthor == rhs.forwardAuthor
    }
}

/// Where a candidate came from, so the screen can draw and open it.
struct FeedCandidateRef: Equatable {
    let source: FeedSource
    let message: FeedMessage
    /// For the item's "why" line (UX spec section 3).
    let mentionsMe: Bool
    let repliesToMe: Bool

    /// Stable across rebuilds: kind, conversation and message id.
    var key: String {
        "\(source.kind == .channel ? "channel" : "group"):\(source.idHex):\(message.messageId)"
    }
}

/// What ``FeedModel/buildInput(_:)`` returns: parallel lists, one ref per
/// candidate.
struct FeedInput {
    let sources: [FeedSource]
    let candidates: [FeedCandidate]
    let refs: [FeedCandidateRef]
}

/// Everything ``FeedModel/buildInput(_:)`` reads.
struct FeedContext {
    let sources: [FeedSource]
    /// Group messages, per group hex id.
    let groupMessages: [String: [FeedMessage]]
    /// The channel-post log, per channel hex id.
    let channelPosts: [String: ChannelPostSlot]
    /// Channels this device publishes to: their posts are the person's own.
    let ownChannels: Set<String>
    /// The person's own username, for mention matching, when known.
    let ownUsername: String?
    /// Signature keys (base64) of accepted contacts.
    let contactKeysB64: Set<String>
    /// The mention rule, `mentionsUsername` from the core.
    let mentions: (String, String) -> Bool
    /// Now, unix milliseconds.
    let nowMs: Int64
}

enum FeedModel {

    /// Oldest message the feed ever looks at: the longest window a rule can
    /// ask for (`lakko-feed` `MAX_WINDOW_DAYS`, 30), which is also how long
    /// the server keeps a message.
    static let maxWindowMs: Int64 = 30 * 86_400_000

    /// Turn the logs into the ranking's input. Rows that cannot be placed (a
    /// system row, a deletion, an unsent row, an unparseable time) are left
    /// out.
    static func buildInput(_ ctx: FeedContext) -> FeedInput {
        let oldest = ctx.nowMs - maxWindowMs
        var candidates: [FeedCandidate] = []
        var refs: [FeedCandidateRef] = []

        for (index, source) in ctx.sources.enumerated() {
            let conversation = UInt32(clamping: index)
            switch source.kind {
            case .channel:
                addChannelPosts(
                    ctx, source: source, conversation: conversation, oldest: oldest,
                    candidates: &candidates, refs: &refs)
            case .group:
                guard let messages = ctx.groupMessages[source.idHex] else { continue }
                var outgoing = Set<Int64>()
                for m in messages where m.direction == .outgoing && m.messageId > 0 {
                    outgoing.insert(m.messageId)
                }
                for m in messages {
                    if m.direction == .system || m.deleted || m.messageId <= 0 { continue }
                    guard let time = m.timeMs, time >= oldest else { continue }
                    let fromSelf = m.direction == .outgoing
                    let reactors = Set(m.reactions.map(\.reactor))
                    let mentionsMe = !fromSelf && ctx.ownUsername.map { ctx.mentions(m.text, $0) } == true
                    let repliesToMe = !fromSelf && m.replyTarget.map { outgoing.contains($0) } == true
                    let fromContact =
                        !fromSelf && m.authorKeyB64.map { ctx.contactKeysB64.contains($0) } == true
                    candidates.append(
                        FeedCandidate(
                            conversation: conversation,
                            messageId: m.messageId,
                            replyTo: m.replyTarget,
                            timeMs: time,
                            fromSelf: fromSelf,
                            fromContact: fromContact,
                            mentionsMe: mentionsMe,
                            repliesToMe: repliesToMe,
                            reactionsDistinct: UInt32(clamping: reactors.count),
                            hasMedia: !m.attachmentLabels.isEmpty,
                            text: m.text
                        ))
                    refs.append(
                        FeedCandidateRef(
                            source: source, message: m, mentionsMe: mentionsMe,
                            repliesToMe: repliesToMe))
                }
            }
        }
        return FeedInput(sources: ctx.sources, candidates: candidates, refs: refs)
    }

    /// A channel's posts as candidates. A post has no author but the channel,
    /// no reply and no reactions; it is the person's own when this device
    /// publishes to the channel.
    private static func addChannelPosts(
        _ ctx: FeedContext,
        source: FeedSource,
        conversation: UInt32,
        oldest: Int64,
        candidates: inout [FeedCandidate],
        refs: inout [FeedCandidateRef]
    ) {
        guard let slot = ctx.channelPosts[source.idHex] else { return }
        let fromSelf = ctx.ownChannels.contains(source.idHex)
        for p in slot.posts {
            guard p.cursor > 0, p.serverTsMs >= oldest, p.cursor <= UInt64(Int64.max) else {
                continue
            }
            let id = Int64(p.cursor)
            let mentionsMe = !fromSelf && ctx.ownUsername.map { ctx.mentions(p.text, $0) } == true
            candidates.append(
                FeedCandidate(
                    conversation: conversation,
                    messageId: id,
                    replyTo: nil,
                    timeMs: p.serverTsMs,
                    fromSelf: fromSelf,
                    fromContact: false,
                    mentionsMe: mentionsMe,
                    repliesToMe: false,
                    reactionsDistinct: 0,
                    hasMedia: false,
                    text: p.text
                ))
            refs.append(
                FeedCandidateRef(
                    source: source,
                    message: FeedMessage(
                        messageId: id,
                        direction: fromSelf ? .outgoing : .incoming,
                        text: p.text,
                        timeMs: p.serverTsMs),
                    mentionsMe: mentionsMe,
                    repliesToMe: false))
        }
    }

    /// The top reactions of a message for its summary: emoji with counts,
    /// most first, at most `limit`. Ties keep first-seen order.
    static func reactionSummary(_ message: FeedMessage, limit: Int = 3) -> [(emoji: String, count: Int)] {
        var order: [String] = []
        var counts: [String: Int] = [:]
        for r in message.reactions {
            if counts[r.emoji] == nil { order.append(r.emoji) }
            counts[r.emoji, default: 0] += 1
        }
        let indexed = order.enumerated().map { (index: $0.offset, emoji: $0.element) }
        return indexed
            .sorted { a, b in
                let ca = counts[a.emoji] ?? 0
                let cb = counts[b.emoji] ?? 0
                return ca != cb ? ca > cb : a.index < b.index
            }
            .prefix(limit)
            .map { (emoji: $0.emoji, count: counts[$0.emoji] ?? 0) }
    }
}

/// The presets the switcher shows, in order (UX spec section 4). Discover
/// joins them with ADR-0059.
enum FeedPresetKey: String, CaseIterable, Equatable {
    case latest
    case popular
    case forMe = "for-me"

    var label: String {
        switch self {
        case .latest: return Localization.text("feed.preset.latest")
        case .popular: return Localization.text("feed.preset.popular")
        case .forMe: return Localization.text("feed.preset.for_me")
        }
    }

    var preset: FeedPreset {
        switch self {
        case .latest: return .latest
        case .popular: return .popular
        case .forMe: return .forMe
        }
    }
}

/// The words on a feed item (`docs/ux/feed.md` sections 3 and 7). Mirror of
/// web `feed/feed-copy.ts`.
enum FeedCopy {

    /// The quiet line saying why an item is where it is, or `nil` when
    /// recency alone put it there. Mentions and replies to the person win
    /// over popularity, because they are about the reader.
    static func whyLine(_ ref: FeedCandidateRef, preset: FeedPresetKey) -> String? {
        if ref.mentionsMe { return Localization.text("feed.why.mentions") }
        if ref.repliesToMe { return Localization.text("feed.why.reply") }
        if preset == .popular && !ref.message.reactions.isEmpty {
            return Localization.text("feed.why.popular", texts: ["source": ref.source.title])
        }
        return nil
    }

    /// What stands in for text when a message carries only attachments.
    static func attachmentLine(_ message: FeedMessage) -> String? {
        let n = message.attachmentLabels.count
        if n == 0 { return nil }
        if n > 1 { return Localization.text("feed.attachments.count", counts: ["count": UInt64(n)]) }
        return message.attachmentLabels[0]
    }

    /// "3 replies", "1 reply".
    static func repliesLabel(_ count: Int) -> String {
        Localization.text("feed.replies.count", counts: ["count": UInt64(max(0, count))])
    }

    /// The item's text, or its attachment line when it has none.
    static func body(_ message: FeedMessage) -> String {
        let trimmed = message.text.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? (attachmentLine(message) ?? "") : message.text
    }
}
