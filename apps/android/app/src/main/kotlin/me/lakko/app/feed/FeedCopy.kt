// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import me.lakko.app.l10n.Strings
import me.lakko.app.l10n.UiText
import uniffi.lakko_ffi_uniffi.FeedPreset

/**
 * The words on a feed item (`docs/ux/feed.md` sections 3 and 7), kept out of
 * the composable so they are tested and stay one sentence each. Mirror of
 * `apps/web/src/feed/feed-copy.ts`.
 */

/** The presets the switcher shows, in order (UX spec section 4). */
public enum class FeedPresetKey(public val labelKey: String, public val preset: FeedPreset) {
    LATEST("feed.preset.latest", FeedPreset.LATEST),
    POPULAR("feed.preset.popular", FeedPreset.POPULAR),
    FOR_ME("feed.preset.for_me", FeedPreset.FOR_ME),
    ;

    /** The chip's label (ADR-0061). */
    public val label: UiText get() = UiText(labelKey)
}

/**
 * The quiet line saying why an item is where it is, or `null` when recency
 * alone put it there. Mentions and replies to the person win over popularity,
 * because they are about the reader.
 */
public fun whyLine(ref: FeedCandidateRef, preset: FeedPresetKey): UiText? {
    if (ref.mentionsMe) return UiText("feed.why.mentions")
    if (ref.repliesToMe) return UiText("feed.why.reply")
    if (preset == FeedPresetKey.POPULAR && ref.message.reactions.isNotEmpty()) {
        return UiText("feed.why.popular", mapOf("source" to ref.source.title))
    }
    return null
}

/** What stands in for text when a message carries only attachments. */
public fun attachmentLine(message: FeedMessage): UiText? {
    val n = message.attachments.size
    if (n == 0) return null
    if (n > 1) return UiText("feed.attachments.count", counts = mapOf("count" to n.toULong()))
    val a = message.attachments[0]
    when (a.kind) {
        "voice" -> return UiText("chat.media.voice_message")
        "video_note" -> return UiText("chat.media.video_message")
        "gif" -> return UiText("chat.media.gif")
    }
    val mime = a.mime.orEmpty()
    if (mime.startsWith("image/")) return UiText("chat.media.photo")
    if (mime.startsWith("video/")) return UiText("chat.media.video")
    return UiText("chat.media.file")
}

/** "3 replies", "1 reply". */
public fun repliesLabel(count: Int): UiText =
    UiText("feed.replies.count", counts = mapOf("count" to count.coerceAtLeast(0).toULong()))

/** The text an item shows: its words, else what its attachments are. */
public fun bodyOf(ref: FeedCandidateRef, strings: Strings): String =
    if (ref.message.text.isNotBlank()) ref.message.text else attachmentLine(ref.message)?.renderWith(strings).orEmpty()
