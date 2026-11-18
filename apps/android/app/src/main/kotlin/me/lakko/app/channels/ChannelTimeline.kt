// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import uniffi.lakko_ffi_uniffi.AttachmentRef

/*
 * ADR-0087 section 13 — a channel's timeline as one conversation shows it.
 *
 * A post's plaintext is the chat envelope: a message with text and
 * attachments, or an ADR-0023 edit / delete op naming a post's server cursor.
 * A reader applies an op from any publisher key the post signature accepts
 * (every post it can open was signed by one), so a publisher can edit or
 * delete another publisher's post. The fold below is pure: the reader decodes
 * each row first ([ChannelRowContent]) and hands the list here.
 */

/** What one decrypted (or not) post row turned out to be. */
public sealed interface ChannelRowContent {
    /** A post: text, attachments, and an invitation card when one rides it. */
    public data class Post(
        val text: String,
        val attachments: List<AttachmentRef>,
        val channelInvite: ByteArray? = null,
        /** ADR-0090 — a poll, checklist, location, contact card or article post. */
        val content: uniffi.lakko_ffi_uniffi.MessageContent? = null,
    ) : ChannelRowContent

    /** ADR-0090 section 4 — the publisher closed the poll at [target],
     *  freezing [tally] into it (absent on an older close). */
    public data class Close(val target: Long, val tally: uniffi.lakko_ffi_uniffi.ChannelPollTally?) : ChannelRowContent

    /** ADR-0090 / UX 8 — a publisher's tick or added item on the checklist at [target]. */
    public data class ChecklistOp(
        val target: Long,
        val add: Boolean,
        val item: uniffi.lakko_ffi_uniffi.ChecklistItemRef?,
        val done: Boolean,
        val text: String,
    ) : ChannelRowContent

    /** ADR-0023 — replace the text of the post at [target]. */
    public data class Edit(val target: Long, val text: String) : ChannelRowContent

    /** ADR-0023 — the post at [target] leaves the timeline for everyone. */
    public data class Delete(val target: Long) : ChannelRowContent

    /** Not shown: the metadata record, a ban list, a malformed record, a
     *  service envelope, anything that is no post. */
    public data object Hidden : ChannelRowContent

    /** A post this device holds no key for (an older epoch, or it does not
     *  open). */
    public data object Unavailable : ChannelRowContent
}

/** One row as fetched: its server cursor, time, epoch and content. */
public data class ChannelRow(
    val cursor: Long,
    val tsMs: Long,
    val epoch: UInt,
    val content: ChannelRowContent,
)

/** One post as the timeline draws it. */
public data class ChannelPostItem(
    val cursor: Long,
    val tsMs: Long,
    val epoch: UInt,
    val text: String,
    val attachments: List<AttachmentRef>,
    val edited: Boolean,
    /** Section 13 — "This post isn't available.": a post this device holds
     *  no key for, after the first one it can read. */
    val unavailable: Boolean = false,
    val channelInvite: ByteArray? = null,
    /** ADR-0090 — the post's content, when it is one. */
    val content: uniffi.lakko_ffi_uniffi.MessageContent? = null,
    /** ADR-0090 section 4 — closed by a publisher, with the frozen tally. */
    val closed: Boolean = false,
    val frozenTally: uniffi.lakko_ffi_uniffi.ChannelPollTally? = null,
    /** ADR-0090 — ops a checklist post collected (publishers' ticks and adds, a close). */
    val ops: List<me.lakko.app.chat.ContentOpRecord> = emptyList(),
)

/** In a channel every op is a publisher's: one stand-in key, always privileged. */
public val CHANNEL_PUBLISHER_KEY: String = java.util.Base64.getEncoder().encodeToString(ByteArray(32))

/** The fold's answer. */
public data class ChannelTimeline(
    /** Oldest first. */
    val posts: List<ChannelPostItem>,
    /** Section 13 — older posts exist that this device cannot read: the line
     *  "Earlier posts aren't available." at the top. */
    val earlierUnavailable: Boolean,
)

/**
 * Fold [rows] into the timeline: ascending by cursor; the newest edit of a
 * post wins; a delete removes it; ops naming no known post are dropped; rows
 * this device cannot read before the first readable post become one line at
 * the top, after it an unavailable placeholder each.
 */
public fun foldChannelTimeline(rows: List<ChannelRow>): ChannelTimeline {
    val ordered = rows.sortedBy { it.cursor }
    val posts = LinkedHashMap<Long, ChannelPostItem>()
    val deleted = HashSet<Long>()
    var seenReadable = false
    var earlier = false
    for (row in ordered) {
        when (val c = row.content) {
            is ChannelRowContent.Post -> {
                seenReadable = true
                posts[row.cursor] = ChannelPostItem(
                    cursor = row.cursor,
                    tsMs = row.tsMs,
                    epoch = row.epoch,
                    text = c.text,
                    attachments = c.attachments,
                    edited = false,
                    channelInvite = c.channelInvite,
                    content = c.content,
                )
            }
            is ChannelRowContent.Close -> {
                val held = posts[c.target]
                if (held != null && held.content != null && !held.closed) {
                    posts[c.target] = held.copy(
                        closed = true,
                        frozenTally = c.tally,
                        ops = held.ops + me.lakko.app.chat.ContentOpRecord(
                            me.lakko.app.chat.ContentOpKind.PollClose, row.cursor, row.tsMs, CHANNEL_PUBLISHER_KEY, true,
                        ),
                    )
                }
            }
            is ChannelRowContent.ChecklistOp -> {
                val held = posts[c.target]
                if (held != null && held.content is uniffi.lakko_ffi_uniffi.MessageContent.Checklist && !held.closed) {
                    posts[c.target] = held.copy(
                        ops = held.ops + me.lakko.app.chat.ContentOpRecord(
                            kind = if (c.add) me.lakko.app.chat.ContentOpKind.ChecklistAdd else me.lakko.app.chat.ContentOpKind.ChecklistTick,
                            opId = row.cursor,
                            createdAtMs = row.tsMs,
                            sender = CHANNEL_PUBLISHER_KEY,
                            privileged = true,
                            item = c.item,
                            done = c.done,
                            text = c.text,
                        ),
                    )
                }
            }
            is ChannelRowContent.Edit -> {
                val held = posts[c.target]
                if (held != null && !held.unavailable) posts[c.target] = held.copy(text = c.text, edited = true)
            }
            is ChannelRowContent.Delete -> {
                if (posts.remove(c.target) != null) deleted += c.target
            }
            ChannelRowContent.Hidden -> Unit
            ChannelRowContent.Unavailable -> {
                if (!seenReadable) {
                    earlier = true
                } else {
                    posts[row.cursor] = ChannelPostItem(
                        cursor = row.cursor,
                        tsMs = row.tsMs,
                        epoch = row.epoch,
                        text = "",
                        attachments = emptyList(),
                        edited = false,
                        unavailable = true,
                    )
                }
            }
        }
    }
    return ChannelTimeline(posts = posts.values.toList(), earlierUnavailable = earlier)
}

/** Section 12 — the one line under a channel's name. [band] is the rough
 *  size of a public channel (`"10k+"`), `null` when not known. */
public fun channelHeaderLineKey(public: Boolean, band: String?): Pair<String, Map<String, String>> = when {
    !public -> "channels.header.private" to emptyMap()
    band == null -> "channels.header.public" to emptyMap()
    else -> "channels.header.public_count" to mapOf("band" to band)
}

/** ADR-0059 section 4 — the size band a count falls in, as shown. */
public fun subscriberBand(count: Long): String = when {
    count >= 100_000 -> "100k+"
    count >= 10_000 -> "10k+"
    count >= 1_000 -> "1k+"
    count >= 100 -> "100+"
    count >= 10 -> "10+"
    else -> count.toString()
}
