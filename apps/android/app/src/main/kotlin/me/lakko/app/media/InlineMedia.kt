// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import uniffi.lakko_ffi_uniffi.AttachmentRef

/**
 * ADR-0047 §1 — how a received attachment is rendered, decided in one place.
 *
 * Port of `apps/web/src/media/inline-media.ts`. Two inputs, two trust levels,
 * and the rule that keeps them apart:
 *
 *   * `kind` is **sender-chosen** — exactly as trustworthy as the file name.
 *     It picks the bubble SHAPE (a voice player, a round video, an animation)
 *     and nothing else.
 *   * Whether the bytes are handed to a decoder or player at all is decided by
 *     the MIME type alone, through the core's allowlist (`inlineMediaType` in
 *     `lakko-attachments`, reached over UniFFI). A `kind: "voice"` carrying
 *     `mime: "text/html"` is a file chip.
 *
 * So a media bubble needs BOTH: the kind asks for the shape, and the
 * allowlisted MIME type must be of the matching media class. Anything else is
 * a file chip, which may still offer an inline preview if (and only if) its
 * MIME type is allowlisted as an image.
 *
 * The allowlist is injected as a function so the rule is testable on the host
 * JVM without loading the native core; production passes [coreAllowlist].
 */
public enum class InlineKind { IMAGE, AUDIO, VIDEO }

/**
 * [PHOTO] and [VIDEO] are the UX spec §10 bubbles for an attachment with NO
 * `kind`: an image on the allowlist is a photo, an allowlisted video is a
 * video. A present-but-unknown `kind` stays a file chip.
 */
public enum class MediaBubble { VOICE, VIDEO_NOTE, GIF, PHOTO, VIDEO, FILE, AUDIO }

public typealias MediaAllowlist = (String) -> InlineKind?

/** The production allowlist: the core's `inlineMediaType`. Loads native code
 *  on first call, so tests never touch it. */
public val coreAllowlist: MediaAllowlist = { mime ->
    when (uniffi.lakko_ffi_uniffi.inlineMediaType(mime)) {
        uniffi.lakko_ffi_uniffi.InlineMediaType.IMAGE -> InlineKind.IMAGE
        uniffi.lakko_ffi_uniffi.InlineMediaType.AUDIO -> InlineKind.AUDIO
        uniffi.lakko_ffi_uniffi.InlineMediaType.VIDEO -> InlineKind.VIDEO
        null -> null
    }
}

/** The allowlisted media class of [mime], or `null`. Fails closed: an absent
 *  MIME or a throwing binding both mean "not inline". */
public fun inlineKindOf(mime: String?, allowlist: MediaAllowlist): InlineKind? {
    if (mime.isNullOrEmpty()) return null
    return try {
        allowlist(mime)
    } catch (_: Throwable) {
        null
    }
}

public data class RenderDecision(
    val bubble: MediaBubble,
    /** The allowlisted media class, independent of the bubble. */
    val inline: InlineKind?,
)

public const val KIND_VOICE: String = "voice"
public const val KIND_VIDEO_NOTE: String = "video_note"
public const val KIND_GIF: String = "gif"

/** ADR-0092 section 2 — a music or audio file (not a voice message). */
public const val KIND_AUDIO: String = "audio"

public fun renderDecision(kind: String?, mime: String?, allowlist: MediaAllowlist): RenderDecision {
    val inline = inlineKindOf(mime, allowlist)
    val bubble = when {
        kind == KIND_VOICE && inline == InlineKind.AUDIO -> MediaBubble.VOICE
        // ADR-0092 section 2 — a music or audio file: title, artist, cover;
        // playable in the app only when its MIME is on the inline list.
        kind == KIND_AUDIO -> MediaBubble.AUDIO
        kind == KIND_VIDEO_NOTE && inline == InlineKind.VIDEO -> MediaBubble.VIDEO_NOTE
        kind == KIND_GIF && (inline == InlineKind.IMAGE || inline == InlineKind.VIDEO) ->
            MediaBubble.GIF
        kind == null && inline == InlineKind.IMAGE -> MediaBubble.PHOTO
        kind == null && inline == InlineKind.VIDEO -> MediaBubble.VIDEO
        else -> MediaBubble.FILE
    }
    return RenderDecision(bubble, inline)
}

public fun renderDecision(ref: AttachmentRef, allowlist: MediaAllowlist): RenderDecision =
    renderDecision(ref.kind, ref.mime, allowlist)

/**
 * The bubble of a message that is NOTHING but one piece of media — no text, no
 * second attachment, not a tombstone — or `null` when the message is anything
 * else.
 *
 * "Nothing but" is the load-bearing half. A voice message sent WITH a caption,
 * or beside another attachment, is an ordinary bubble that happens to contain a
 * player: it has a paragraph to sit under, so it keeps the bubble's own meta
 * row ([BubbleMetaSlot.BUBBLE_ROW]). Only the lone one gets a slot of its own.
 */
public fun soleMediaBubble(
    deleted: Boolean,
    text: String,
    attachments: List<AttachmentRef>,
    allowlist: MediaAllowlist,
): MediaBubble? {
    if (deleted || text.isNotEmpty() || attachments.size != 1) return null
    return renderDecision(attachments[0], allowlist).bubble
}

/**
 * Where a message's meta line — the time, the edited marker, the delivery
 * ticks — is drawn.
 *
 * Four answers, because four bubble shapes have four different places to put
 * it, and the rule is stated once here rather than as conditionals at the three
 * call sites that used to disagree about it:
 *
 *  * [BUBBLE_ROW] — a row under the content, inside the bubble's surface. The
 *    default, and where a paragraph wants it.
 *  * [VOICE_LINE] — the voice row's own quiet line, at its trailing end (UX
 *    spec §2). A voice message is a ROW, not a paragraph: putting the length of
 *    the recording and the time it was sent on two different levels makes the
 *    bubble taller than it has any reason to be and reads as two unrelated
 *    facts.
 *  * [MEDIA_PILL] — a translucent pill over the media (UX spec §10), because a
 *    lone photo or video is drawn edge to edge with no chrome to carry a row.
 *  * [NOTE_PILL] — a translucent pill inside the chromeless video-note circle,
 *    bottom trailing (UX spec §3, amended 2026-11-22): the circle has no
 *    surface to carry a row, and a line under it read as loose text floating
 *    under the video.
 */
public enum class BubbleMetaSlot { BUBBLE_ROW, VOICE_LINE, MEDIA_PILL, NOTE_PILL }

/** The meta slot for a message whose sole media bubble is [sole]
 *  ([soleMediaBubble], `null` for every other message). */
public fun bubbleMetaSlot(sole: MediaBubble?): BubbleMetaSlot = when (sole) {
    MediaBubble.VOICE -> BubbleMetaSlot.VOICE_LINE
    MediaBubble.PHOTO, MediaBubble.VIDEO -> BubbleMetaSlot.MEDIA_PILL
    MediaBubble.VIDEO_NOTE -> BubbleMetaSlot.NOTE_PILL
    // A GIF keeps the row: it is a bubble with chrome, and a pill over an
    // animation that may be 40dp tall would cover it.
    MediaBubble.GIF, MediaBubble.FILE, MediaBubble.AUDIO, null -> BubbleMetaSlot.BUBBLE_ROW
}
