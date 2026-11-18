// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import me.lakko.app.l10n.Strings
import me.lakko.app.l10n.UiText
import me.lakko.app.media.MediaAllowlist
import me.lakko.app.media.MediaBubble
import me.lakko.app.media.renderDecision
import uniffi.lakko_ffi_uniffi.AttachmentRef
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

/** The text a row with no bubbles shows (a folded reply, the editor's
 *  preview): its words, else what its attachments are. */
public fun bodyOf(ref: FeedCandidateRef, strings: Strings): String =
    when {
        ref.message.text.isNotBlank() -> ref.message.text
        ref.message.content != null -> contentLine(ref.message.content, strings)
        else -> attachmentLine(ref.message)?.renderWith(strings).orEmpty()
    }

/** ADR-0090 — the one line a poll, checklist, location, contact card or
 *  article shows in the feed (the chat list's summary, `contentSummary`). */
public fun contentLine(content: uniffi.lakko_ffi_uniffi.MessageContent, strings: Strings): String {
    val (key, args) = me.lakko.app.chat.contentSummary(content)
    return UiText(key, args).renderWith(strings)
}

/** Whether [attachment] draws as a media bubble, by the timeline's own rule
 *  ([renderDecision]: the MIME allowlist decides, ADR-0047 section 1). */
private fun drawsAsBubble(attachment: FeedAttachment, allowlist: MediaAllowlist): Boolean =
    attachment.ref != null &&
        renderDecision(attachment.kind, attachment.mime, allowlist).bubble != MediaBubble.FILE

/** The attachments an item draws as media bubbles, in message order. A file
 *  is not one: it stays the one-line text (UX spec section 3). */
public fun mediaRefsOf(message: FeedMessage, allowlist: MediaAllowlist): List<AttachmentRef> =
    // An article's images are its own, drawn inside it when it is opened.
    if (message.content != null) emptyList() else message.attachments.filter { drawsAsBubble(it, allowlist) }.mapNotNull(FeedAttachment::ref)

/**
 * The text line of an item that draws its media ([mediaRefsOf]): its words,
 * else what is left once the bubbles say the rest, so a media-only message has
 * no text line at all.
 */
public fun itemBodyOf(ref: FeedCandidateRef, strings: Strings, allowlist: MediaAllowlist): String {
    val message = ref.message
    if (message.text.isNotBlank()) return message.text
    message.content?.let { return contentLine(it, strings) }
    val files = message.attachments.filterNot { drawsAsBubble(it, allowlist) }
    return attachmentLine(message.copy(attachments = files))?.renderWith(strings).orEmpty()
}
