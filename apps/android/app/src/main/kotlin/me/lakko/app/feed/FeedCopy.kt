// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import uniffi.lakko_ffi_uniffi.FeedPreset

/**
 * The words on a feed item (`docs/ux/feed.md` sections 3 and 7), kept out of
 * the composable so they are tested and stay one sentence each. Mirror of
 * `apps/web/src/feed/feed-copy.ts`.
 */

/** The presets the switcher shows, in order (UX spec section 4). */
public enum class FeedPresetKey(public val label: String, public val preset: FeedPreset) {
    LATEST("Latest", FeedPreset.LATEST),
    POPULAR("Popular", FeedPreset.POPULAR),
    FOR_ME("For me", FeedPreset.FOR_ME),
}

/**
 * The quiet line saying why an item is where it is, or `null` when recency
 * alone put it there. Mentions and replies to the person win over popularity,
 * because they are about the reader.
 */
public fun whyLine(ref: FeedCandidateRef, preset: FeedPresetKey): String? {
    if (ref.mentionsMe) return "Mentions you"
    if (ref.repliesToMe) return "Reply to your message"
    if (preset == FeedPresetKey.POPULAR && ref.message.reactions.isNotEmpty()) {
        return "Popular in ${ref.source.title}"
    }
    return null
}

/** What stands in for text when a message carries only attachments. */
public fun attachmentLine(message: FeedMessage): String? {
    val n = message.attachments.size
    if (n == 0) return null
    if (n > 1) return "$n attachments"
    val a = message.attachments[0]
    when (a.kind) {
        "voice" -> return "Voice message"
        "video_note" -> return "Video message"
        "gif" -> return "GIF"
    }
    val mime = a.mime.orEmpty()
    if (mime.startsWith("image/")) return "Photo"
    if (mime.startsWith("video/")) return "Video"
    return "File"
}

/** "3 replies", "1 reply". */
public fun repliesLabel(count: Int): String = if (count == 1) "1 reply" else "$count replies"

/** The text an item shows: its words, else what its attachments are. */
public fun bodyOf(ref: FeedCandidateRef): String =
    if (ref.message.text.isNotBlank()) ref.message.text else attachmentLine(ref.message).orEmpty()
