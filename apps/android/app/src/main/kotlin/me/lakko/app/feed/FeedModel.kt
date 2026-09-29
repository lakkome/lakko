// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

/**
 * The feed's input, assembled from what this device already holds (ADR-0058
 * section 1, `docs/ux/feed.md` section 9). Mirror of
 * `apps/web/src/feed/feed-model.ts`.
 *
 * The ranking is not here: it is `lakko-feed` in Rust, reached through
 * [FeedCore]. This file only turns the per-group timelines and the
 * channel-post log into the flat conversation and candidate lists the ranking
 * reads, and keeps a reference from each candidate back to the row it came
 * from. Plain data in, plain data out: no Compose, no FFI, no storage.
 *
 * One-to-one chats never reach it: the caller passes groups and channels only
 * (UX spec section 3).
 */

public enum class FeedSourceKind { GROUP, CHANNEL }

/** One group or channel the feed may take items from. */
public data class FeedSource(
    val kind: FeedSourceKind,
    /** The group id hex (the message-log key), or the 32-char channel id. */
    val idHex: String,
    /** What the conversation list calls it. */
    val title: String,
    /** Muted in this account's conversation flags. */
    val muted: Boolean,
    /** People in a group, for weighing reactions against its size; 0 for a
     *  channel, whose posts carry no reactions. */
    val audience: Int,
)

/** One attachment as the feed draws it. */
public data class FeedAttachment(val kind: String?, val mime: String?)

/** One reaction as the feed counts it. */
public data class FeedReaction(val reactorKeyB64: String, val emoji: String)

/** A group message or a channel post in the shape the feed reads. */
public data class FeedMessage(
    val messageId: Long,
    /** `"incoming"`, `"outgoing"` or `"system"`. */
    val direction: String,
    val text: String,
    val attachments: List<FeedAttachment> = emptyList(),
    /** ISO-8601; parsed with [parseFeedTimeMs]. */
    val createdAt: String,
    val authorKeyB64: String? = null,
    val edited: Boolean = false,
    val deleted: Boolean = false,
    val reactions: List<FeedReaction> = emptyList(),
    val replyTarget: Long? = null,
    val forwardAuthor: String? = null,
)

/** One candidate in the shape `rankFeed` takes, before it crosses the FFI. */
public data class FeedCandidateInput(
    val conversation: Int,
    val messageId: Long,
    val replyTo: Long?,
    val timeMs: Long,
    val fromSelf: Boolean,
    val fromContact: Boolean,
    val mentionsMe: Boolean,
    val repliesToMe: Boolean,
    val reactionsDistinct: Int,
    val hasMedia: Boolean,
    val text: String,
)

/** Where a candidate came from, so the screen can draw and open it. */
public data class FeedCandidateRef(
    val source: FeedSource,
    val message: FeedMessage,
    /** For the item's "why" line (UX spec section 3). */
    val mentionsMe: Boolean,
    val repliesToMe: Boolean,
)

/** What [buildFeedInput] returns: parallel lists, one ref per candidate. */
public data class FeedInput(
    val sources: List<FeedSource>,
    val candidates: List<FeedCandidateInput>,
    val refs: List<FeedCandidateRef>,
)

/** Everything [buildFeedInput] reads. */
public data class FeedContext(
    val sources: List<FeedSource>,
    /** Group timelines, keyed on the group id hex. */
    val groupMessages: Map<String, List<FeedMessage>>,
    /** The channel-post log, keyed on the channel id. */
    val channelPosts: Map<String, ChannelPostSlot>,
    /** Channels this device publishes to: their posts are the person's own. */
    val ownChannels: Set<String>,
    /** The person's own username, for mention matching, when known. */
    val ownUsername: String?,
    /** Signature keys (standard base64) of accepted contacts. */
    val contactKeysB64: Set<String>,
    /** The mention rule, `mentionsUsername` from the core. */
    val mentions: (text: String, username: String) -> Boolean,
    val nowMs: Long,
)

/**
 * Oldest message the feed ever looks at: the longest window a rule can ask for
 * (`lakko-feed` `MAX_WINDOW_DAYS`, 30), which is also how long the server keeps
 * a message.
 */
public const val FEED_MAX_WINDOW_MS: Long = 30L * 86_400_000L

/** Parse an ISO-8601 instant (`...Z` or with an offset) to unix ms; `null`
 *  when it does not parse. */
public fun parseFeedTimeMs(value: String): Long? = try {
    java.time.Instant.parse(value).toEpochMilli()
} catch (_: java.time.format.DateTimeParseException) {
    try {
        java.time.OffsetDateTime.parse(value).toInstant().toEpochMilli()
    } catch (_: java.time.format.DateTimeParseException) {
        null
    }
}

private fun addChannelPosts(
    ctx: FeedContext,
    source: FeedSource,
    conversation: Int,
    oldest: Long,
    candidates: MutableList<FeedCandidateInput>,
    refs: MutableList<FeedCandidateRef>,
) {
    val slot = ctx.channelPosts[source.idHex] ?: return
    val fromSelf = source.idHex in ctx.ownChannels
    for (p in slot.posts) {
        if (p.cursor <= 0 || p.serverTsMs < oldest) continue
        val mentionsMe = !fromSelf && ctx.ownUsername != null && ctx.mentions(p.text, ctx.ownUsername)
        candidates += FeedCandidateInput(
            conversation = conversation,
            messageId = p.cursor,
            replyTo = null,
            timeMs = p.serverTsMs,
            fromSelf = fromSelf,
            fromContact = false,
            mentionsMe = mentionsMe,
            repliesToMe = false,
            reactionsDistinct = 0,
            hasMedia = false,
            text = p.text,
        )
        val message = FeedMessage(
            messageId = p.cursor,
            direction = if (fromSelf) "outgoing" else "incoming",
            text = p.text,
            createdAt = java.time.Instant.ofEpochMilli(p.serverTsMs).toString(),
        )
        refs += FeedCandidateRef(source, message, mentionsMe, repliesToMe = false)
    }
}

/** Turn the logs into the ranking's input. Rows that cannot be placed (a
 *  system row, a deletion, a non-positive id, an unparseable time) are left
 *  out. */
public fun buildFeedInput(ctx: FeedContext): FeedInput {
    val oldest = ctx.nowMs - FEED_MAX_WINDOW_MS
    val candidates = ArrayList<FeedCandidateInput>()
    val refs = ArrayList<FeedCandidateRef>()
    ctx.sources.forEachIndexed { conversation, source ->
        if (source.kind == FeedSourceKind.CHANNEL) {
            addChannelPosts(ctx, source, conversation, oldest, candidates, refs)
            return@forEachIndexed
        }
        val messages = ctx.groupMessages[source.idHex] ?: return@forEachIndexed
        val outgoing = messages.filter { it.direction == "outgoing" }.map { it.messageId }.toSet()
        for (m in messages) {
            if (m.direction == "system" || m.deleted || m.messageId <= 0) continue
            val time = parseFeedTimeMs(m.createdAt) ?: continue
            if (time < oldest) continue
            val fromSelf = m.direction == "outgoing"
            val author = m.authorKeyB64
            val reactors = m.reactions.map { it.reactorKeyB64 }.toSet()
            val mentionsMe = !fromSelf && ctx.ownUsername != null && ctx.mentions(m.text, ctx.ownUsername)
            val replyTarget = m.replyTarget?.takeIf { it > 0 }
            val repliesToMe = !fromSelf && replyTarget != null && replyTarget in outgoing
            candidates += FeedCandidateInput(
                conversation = conversation,
                messageId = m.messageId,
                replyTo = replyTarget,
                timeMs = time,
                fromSelf = fromSelf,
                fromContact = !fromSelf && author != null && author in ctx.contactKeysB64,
                mentionsMe = mentionsMe,
                repliesToMe = repliesToMe,
                reactionsDistinct = reactors.size,
                hasMedia = m.attachments.isNotEmpty(),
                text = m.text,
            )
            refs += FeedCandidateRef(source, m, mentionsMe, repliesToMe)
        }
    }
    return FeedInput(ctx.sources, candidates, refs)
}

/** One reaction with its count, for an item's summary. */
public data class ReactionCount(val emoji: String, val count: Int)

/** The top reactions of a message: emoji with counts, most first, at most
 *  [limit]. Ties keep first-seen order. */
public fun reactionSummary(message: FeedMessage, limit: Int = 3): List<ReactionCount> {
    val counts = LinkedHashMap<String, Int>()
    for (r in message.reactions) counts[r.emoji] = (counts[r.emoji] ?: 0) + 1
    return counts.entries
        .map { ReactionCount(it.key, it.value) }
        .sortedByDescending { it.count }
        .take(limit)
}
