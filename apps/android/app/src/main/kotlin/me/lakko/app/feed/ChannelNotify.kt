// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import uniffi.lakko_ffi_uniffi.ChannelNotifyBatch
import uniffi.lakko_ffi_uniffi.ChannelNotifyContext
import uniffi.lakko_ffi_uniffi.ChannelPostCandidate
import uniffi.lakko_ffi_uniffi.ChannelReadKind

/**
 * Channel post notifications (`docs/ux/channel-notifications.md`). The
 * decision is the shared rule (`lakko_ui_rules::channel_notify`, through
 * UniFFI); this file only builds its inputs from what a channel read
 * produced. Pure: the rule is injected, so the host JVM suite runs the wiring
 * with no native library. Mirror of `apps/web/src/feed/channel-notify.ts`.
 *
 * There is no push for channels yet: a post is noticed when this device
 * reads the channel, in the foreground sync ([ChannelPostSync]) and in the
 * periodic background work ([ChannelPostWorker]).
 */

/**
 * What kind of read produced a channel's new records. A channel with no slot
 * in the post log before this read is being read from its start: the first
 * read after subscribing when it has no read marker yet (the marker is
 * seeded right after that read, [seedChannelRead]), else a refetch of
 * history (the log was dropped by a format change, or the slot was lost while
 * its marker survived). As web.
 */
public fun channelReadKind(slotBefore: ChannelPostSlot?, readMarker: Long?): ChannelReadKind = when {
    slotBefore != null -> ChannelReadKind.ONGOING
    readMarker == null -> ChannelReadKind.FIRST_AFTER_SUBSCRIBE
    else -> ChannelReadKind.HISTORY
}

/** One channel's read, as the sync hands it over. */
public data class ChannelRead(
    val channelIdHex: String,
    /** The channel's slot before this read; `null` when it had none. */
    val slotBefore: ChannelPostSlot?,
    /** The slot after it. */
    val slotAfter: ChannelPostSlot,
    /** The channel's read marker before this read; `null` when none. */
    val readMarker: Long?,
)

/** What the device knows of the channel when the read is judged. */
public data class ChannelNotifyFacts(
    val muted: Boolean,
    /** The channel screen is open in front of the person. */
    val openAndVisible: Boolean,
    /** This device publishes to the channel: every post counts as the
     *  person's own (a post signing key is random per device and channel, so
     *  a post from the person's other device cannot be told from a
     *  co-publisher's; the channel's row counts none of them as new either). */
    val own: Boolean,
    /** The cursor stored after this device's last read of the channel. */
    val lastNotified: Long,
    val nowMs: Long,
)

/** The decision for one read. */
public data class ChannelNotifyDecision(
    /** The post to show, the newest that notifies; `null` for none. */
    val post: PersistedChannelPost?,
    /** The cursor to store for the channel (always stored). */
    val lastNotified: Long,
)

/** The shared rule, injected so the host suite needs no native library. */
public fun interface ChannelNotifyRule {
    public fun postsToNotify(context: ChannelNotifyContext, posts: List<ChannelPostCandidate>): ChannelNotifyBatch
}

/** The rule in the core. */
public val coreChannelNotifyRule: ChannelNotifyRule = ChannelNotifyRule { context, posts ->
    uniffi.lakko_ffi_uniffi.channelPostsToNotify(context, posts)
}

/**
 * Judge one channel's read by the shared rule. The records are the posts the
 * read added and, when the read also passed over records that are not posts
 * (a metadata record, a ban list, an edit), one non-post record at the slot's
 * new cursor, so the stored cursor moves past them too. As web.
 */
public fun decideChannelRead(
    rule: ChannelNotifyRule,
    read: ChannelRead,
    facts: ChannelNotifyFacts,
): ChannelNotifyDecision {
    val before = read.slotBefore?.cursor ?: 0L
    val fresh = read.slotAfter.posts.filter { it.cursor > before }
    val candidates = ArrayList<ChannelPostCandidate>(fresh.size + 1)
    for (p in fresh) {
        candidates += ChannelPostCandidate(
            cursor = p.cursor.coerceAtLeast(0L).toULong(),
            isPost = true,
            own = facts.own,
            serverTsMs = p.serverTsMs.coerceAtLeast(0L).toULong(),
        )
    }
    val newCursor = read.slotAfter.cursor
    if (newCursor > before && fresh.none { it.cursor == newCursor }) {
        candidates += ChannelPostCandidate(
            cursor = newCursor.toULong(),
            isPost = false,
            own = facts.own,
            serverTsMs = 0UL,
        )
    }
    val context = ChannelNotifyContext(
        muted = facts.muted,
        openAndVisible = facts.openAndVisible,
        readMarker = (read.readMarker ?: 0L).coerceAtLeast(0L).toULong(),
        lastNotified = facts.lastNotified.coerceAtLeast(0L).toULong(),
        readKind = channelReadKind(read.slotBefore, read.readMarker),
        nowMs = facts.nowMs.coerceAtLeast(0L).toULong(),
    )
    val batch = rule.postsToNotify(context, candidates)
    val last = batch.notify.lastOrNull()?.toInt()
    val post = last?.let { fresh.getOrNull(it) }
    val stored = batch.lastNotified.coerceAtMost(Long.MAX_VALUE.toULong()).toLong()
    return ChannelNotifyDecision(post = post, lastNotified = stored)
}

/** Notifications of one channel replace each other: one tag per channel. */
public fun channelNotificationTag(channelIdHex: String): String = "channel:${channelIdHex.lowercase()}"

/**
 * The notification's body: the channel row's second line for that post (its
 * first non-blank text line, else the attachment's kind), else [fallback]
 * (the generic "New message").
 */
public fun channelNotificationBody(
    post: PersistedChannelPost,
    fallback: String,
    previewLine: (PersistedChannelPost) -> String?,
): String {
    val line = post.text.lineSequence().map { it.trim() }.firstOrNull { it.isNotEmpty() }.orEmpty()
    return previewLine(post.copy(text = line))?.takeIf { it.isNotBlank() } ?: fallback
}
