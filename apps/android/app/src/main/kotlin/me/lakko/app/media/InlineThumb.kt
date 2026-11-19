// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import uniffi.lakko_ffi_uniffi.AttachmentRef

/*
 * ADR-0047 section 6 — the micro thumbnail carried inside the message: a JPEG
 * whose longest side is at most [INLINE_THUMB_LONGEST_SIDE] px and which is at
 * most [INLINE_THUMB_MAX_BYTES] bytes, set only on the first photo or video of
 * a message. It draws the notification picture (no download, so a process the
 * wake started shows it too), the chat list row's thumbnail, and the first
 * mosaic tile until the sent `thumb` arrives.
 *
 * The rules here are pure (no Android types) so they run on the host JVM.
 */

/** The micro thumbnail's longest side, in px (ADR-0047 section 6). */
public const val INLINE_THUMB_LONGEST_SIDE: Int = 96

/** The micro thumbnail's byte bound (`MAX_INLINE_THUMB_BYTES` in the core). */
public const val INLINE_THUMB_MAX_BYTES: Int = 4096

/** JPEG qualities tried in order until the micro thumbnail fits. */
public val INLINE_THUMB_QUALITY_LADDER: List<Int> = listOf(75, 65, 55, 45, 35, 25, 15, 10)

/**
 * Whether [bytes] may be used as a micro thumbnail: non-empty, at most
 * [INLINE_THUMB_MAX_BYTES], and starting `FF D8 FF` (a JPEG). Every reader
 * checks this again, whatever the core already dropped.
 */
public fun isInlineThumbJpeg(bytes: ByteArray?): Boolean =
    bytes != null &&
        bytes.size in 3..INLINE_THUMB_MAX_BYTES &&
        bytes[0] == 0xFF.toByte() &&
        bytes[1] == 0xD8.toByte() &&
        bytes[2] == 0xFF.toByte()

/** [bytes] when it passes [isInlineThumbJpeg], else `null`. */
public fun inlineThumbOrNull(bytes: ByteArray?): ByteArray? = bytes?.takeIf(::isInlineThumbJpeg)

/**
 * The sender's quality ladder: encode with [compress] at falling qualities
 * and keep the first result that fits [INLINE_THUMB_MAX_BYTES] and is a JPEG.
 * `null` when none does: the message goes without a micro thumbnail. A
 * throwing or empty encode counts as no result for that quality.
 */
public fun inlineThumbUnderBound(
    qualities: List<Int> = INLINE_THUMB_QUALITY_LADDER,
    compress: (quality: Int) -> ByteArray?,
): ByteArray? {
    for (q in qualities) {
        val bytes = runCatching { compress(q) }.getOrNull() ?: continue
        if (isInlineThumbJpeg(bytes)) return bytes
    }
    return null
}

/**
 * The refs as they go out: the micro thumbnail stays only on the first ref
 * whose MIME is an allowlisted image or video (the core's
 * `inline_thumb_carrier` rule) and is dropped from every other ref, so what
 * the sender keeps in its own log is what the receivers get.
 */
public fun keepInlineThumbOnCarrier(refs: List<AttachmentRef>, allowlist: MediaAllowlist): List<AttachmentRef> {
    if (refs.none { it.inlineThumb != null }) return refs
    val carrier = refs.indexOfFirst { ref ->
        val kind = inlineKindOf(ref.mime, allowlist)
        kind == InlineKind.IMAGE || kind == InlineKind.VIDEO
    }
    return refs.mapIndexed { index, ref ->
        when {
            index == carrier -> if (isInlineThumbJpeg(ref.inlineThumb)) ref else ref.copy(inlineThumb = null)
            ref.inlineThumb == null -> ref
            else -> ref.copy(inlineThumb = null)
        }
    }
}
