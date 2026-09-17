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
public enum class MediaBubble { VOICE, VIDEO_NOTE, GIF, PHOTO, VIDEO, FILE }

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

public fun renderDecision(kind: String?, mime: String?, allowlist: MediaAllowlist): RenderDecision {
    val inline = inlineKindOf(mime, allowlist)
    val bubble = when {
        kind == KIND_VOICE && inline == InlineKind.AUDIO -> MediaBubble.VOICE
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
