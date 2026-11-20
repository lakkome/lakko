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
    /** ADR-0089 section 2 — the post's wire as fetched, what a report of it includes. */
    val wire: ByteArray? = null,
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
    /** ADR-0089 section 2 — the post's wire, for a report of it. */
    val wire: ByteArray? = null,
)

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
                    wire = row.wire,
                )
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
