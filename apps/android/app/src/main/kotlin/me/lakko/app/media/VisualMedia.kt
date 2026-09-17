// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

/**
 * UX spec `docs/ux/media-messages.md` §10 — the pure rules behind photo and
 * video bubbles, kept free of Android types so they run as host JUnit tests.
 *
 *   * [fitWithin] — the sender's `width` / `height` (scaled to fit 4096) and
 *     the thumb's pixel size (longest side 320).
 *   * [compressUnderBudget] — the JPEG quality ladder for the thumb.
 *   * [mediaBubbleBox] — the receiver's fixed bubble size, known before any
 *     byte is downloaded.
 */

/** §10 Sending — `width` / `height` are scaled down to fit this. */
public const val MEDIA_DIMENSION_CAP: Int = 4096

/** §10 Sending — the thumb's longest side, in px. */
public const val THUMB_LONGEST_SIDE: Int = 320

/** §10 Sending — the size a thumb aims to stay under. */
public const val THUMB_TARGET_BYTES: Int = 64 * 1024

/** §10 Sending — the wire's hard cap on a thumb blob (ADR-0047 §1). */
public const val THUMB_HARD_CAP_BYTES: Int = 256 * 1024

/** §10 Receiving — the bubble box, in dp. */
public const val MEDIA_BUBBLE_MAX_WIDTH_DP: Float = 260f
public const val MEDIA_BUBBLE_MAX_HEIGHT_DP: Float = 360f

/** A bubble never collapses below this on either side, however extreme the
 *  sender's aspect ratio; the content is cropped instead. */
public const val MEDIA_BUBBLE_MIN_SIDE_DP: Float = 80f

/** Pixel size, both sides >= 1. */
public data class PixelSize(val width: Int, val height: Int)

/**
 * Scale [width] x [height] down proportionally so its longest side is at most
 * [maxSide]. Never scales up. `null` for a non-positive input.
 */
public fun fitWithin(width: Int, height: Int, maxSide: Int): PixelSize? {
    if (width <= 0 || height <= 0 || maxSide <= 0) return null
    val longest = maxOf(width, height)
    if (longest <= maxSide) return PixelSize(width, height)
    val scale = maxSide.toDouble() / longest
    return PixelSize(
        width = maxOf(1, Math.round(width * scale).toInt()).coerceAtMost(maxSide),
        height = maxOf(1, Math.round(height * scale).toInt()).coerceAtMost(maxSide),
    )
}

/** JPEG qualities tried in order for a thumb. */
public val THUMB_QUALITY_LADDER: List<Int> = listOf(85, 75, 65, 50, 35, 20)

/**
 * Encode with [compress] at falling qualities until the result is under
 * [target]. If no quality reaches [target], the smallest result is accepted
 * when it is still under [hardCap]; otherwise `null` (send without a thumb).
 * A throwing or empty encode counts as no result for that quality.
 */
public fun compressUnderBudget(
    target: Int = THUMB_TARGET_BYTES,
    hardCap: Int = THUMB_HARD_CAP_BYTES,
    qualities: List<Int> = THUMB_QUALITY_LADDER,
    compress: (quality: Int) -> ByteArray?,
): ByteArray? {
    var smallest: ByteArray? = null
    for (q in qualities) {
        val bytes = runCatching { compress(q) }.getOrNull()
        if (bytes == null || bytes.isEmpty()) continue
        if (bytes.size < target) return bytes
        if (smallest == null || bytes.size < smallest.size) smallest = bytes
    }
    return smallest?.takeIf { it.size <= hardCap }
}

/** A bubble size in dp. */
public data class BubbleBox(val widthDp: Float, val heightDp: Float)

/**
 * §10 Receiving — the fixed bubble size. The aspect comes from the ref's
 * `width` / `height`, else the thumb's, else 4:3. The box is at most
 * [maxWidth] wide and [maxHeight] high, aspect preserved, and at least
 * [MEDIA_BUBBLE_MIN_SIDE_DP] on each side.
 */
public fun mediaBubbleBox(
    width: Int?,
    height: Int?,
    thumbWidth: Int?,
    thumbHeight: Int?,
    maxWidth: Float = MEDIA_BUBBLE_MAX_WIDTH_DP,
    maxHeight: Float = MEDIA_BUBBLE_MAX_HEIGHT_DP,
): BubbleBox {
    val aspect = when {
        width != null && height != null && width > 0 && height > 0 -> width.toFloat() / height
        thumbWidth != null && thumbHeight != null && thumbWidth > 0 && thumbHeight > 0 ->
            thumbWidth.toFloat() / thumbHeight
        else -> 4f / 3f
    }
    var w = maxWidth
    var h = w / aspect
    if (h > maxHeight) {
        h = maxHeight
        w = h * aspect
    }
    return BubbleBox(
        widthDp = w.coerceIn(MEDIA_BUBBLE_MIN_SIDE_DP, maxWidth),
        heightDp = h.coerceIn(MEDIA_BUBBLE_MIN_SIDE_DP, maxHeight),
    )
}

/** Whether an EXIF / container rotation swaps width and height. */
public fun rotationSwapsSides(degrees: Int): Boolean {
    val d = ((degrees % 360) + 360) % 360
    return d == 90 || d == 270
}
