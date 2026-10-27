// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

/*
 * docs/ux/profile-screen.md §2.2 — choosing the square. The picture fills
 * the screen behind a circle of [viewport] pixels; the person drags to move
 * it and zooms between "fills the circle" (1x) and 4x. Pure arithmetic, so
 * the host JVM checks every rule.
 */

/** The zoom range of §2.2. */
public const val CROP_MIN_ZOOM: Float = 1f
public const val CROP_MAX_ZOOM: Float = 4f

/**
 * Where the picture sits under the circle.
 *
 * @param zoom 1..4, where 1 makes the picture's short side fill the circle.
 * @param offsetX the picture centre's horizontal offset from the circle
 *   centre, screen pixels (positive = moved right).
 * @param offsetY the same, vertically (positive = moved down).
 */
public data class CropPosition(val zoom: Float = 1f, val offsetX: Float = 0f, val offsetY: Float = 0f)

/** A square in source pixels. */
public data class CropSquare(val left: Int, val top: Int, val side: Int)

/** Screen pixels per source pixel at [zoom]. */
public fun cropScale(sourceWidth: Int, sourceHeight: Int, viewport: Float, zoom: Float): Float {
    val short = min(sourceWidth, sourceHeight).coerceAtLeast(1)
    return viewport / short * zoom.coerceIn(CROP_MIN_ZOOM, CROP_MAX_ZOOM)
}

/**
 * [position] moved so the circle is always covered by the picture: the
 * zoom clamped to 1..4, each offset to at most half the overhang.
 */
public fun clampCrop(sourceWidth: Int, sourceHeight: Int, viewport: Float, position: CropPosition): CropPosition {
    val zoom = position.zoom.coerceIn(CROP_MIN_ZOOM, CROP_MAX_ZOOM)
    val scale = cropScale(sourceWidth, sourceHeight, viewport, zoom)
    val maxX = max(0f, (sourceWidth * scale - viewport) / 2f)
    val maxY = max(0f, (sourceHeight * scale - viewport) / 2f)
    return CropPosition(zoom, position.offsetX.coerceIn(-maxX, maxX), position.offsetY.coerceIn(-maxY, maxY))
}

/** A drag of ([dx], [dy]) screen pixels and a pinch of factor [zoomBy]
 *  applied to [position], clamped. */
public fun moveCrop(
    sourceWidth: Int,
    sourceHeight: Int,
    viewport: Float,
    position: CropPosition,
    dx: Float,
    dy: Float,
    zoomBy: Float = 1f,
): CropPosition {
    val zoom = (position.zoom * zoomBy).coerceIn(CROP_MIN_ZOOM, CROP_MAX_ZOOM)
    val ratio = if (position.zoom > 0f) zoom / position.zoom else 1f
    // Zooming about the circle centre scales the offset with the picture.
    return clampCrop(
        sourceWidth,
        sourceHeight,
        viewport,
        CropPosition(zoom, position.offsetX * ratio + dx, position.offsetY * ratio + dy),
    )
}

/** The square of the source the circle shows at [position]. */
public fun cropSquare(sourceWidth: Int, sourceHeight: Int, viewport: Float, position: CropPosition): CropSquare {
    val p = clampCrop(sourceWidth, sourceHeight, viewport, position)
    val scale = cropScale(sourceWidth, sourceHeight, viewport, p.zoom)
    val sideF = viewport / scale
    val side = sideF.roundToInt().coerceIn(1, min(sourceWidth, sourceHeight).coerceAtLeast(1))
    val cx = sourceWidth / 2f - p.offsetX / scale
    val cy = sourceHeight / 2f - p.offsetY / scale
    val left = (cx - side / 2f).roundToInt().coerceIn(0, max(0, sourceWidth - side))
    val top = (cy - side / 2f).roundToInt().coerceIn(0, max(0, sourceHeight - side))
    return CropSquare(left, top, side)
}

/** The output side for a square of [cropSide] source pixels: never larger
 *  than the source square, never larger than [maxSide]. */
public fun outputSide(cropSide: Int, maxSide: Int): Int = min(cropSide, maxSide).coerceAtLeast(1)

/** The JPEG qualities tried, highest first (§2.2: re-encode with decreasing
 *  quality until it fits). */
public val JPEG_QUALITIES: List<Int> = listOf(90, 82, 74, 66, 58, 50, 42, 34, 26)

/**
 * The first encoding under [maxBytes], trying [JPEG_QUALITIES] in order, or
 * `null` when even the lowest is too large. [encode] compresses at a quality.
 */
public fun encodeUnderBudget(maxBytes: Int, encode: (quality: Int) -> ByteArray): ByteArray? {
    for (q in JPEG_QUALITIES) {
        val bytes = encode(q)
        if (bytes.isNotEmpty() && bytes.size <= maxBytes) return bytes
    }
    return null
}

/**
 * The 10-second window of a video of [durationMs] starting at [startMs]:
 * the start clamped so the window fits, and the window's length (at most
 * [maxMs]).
 */
public fun videoWindow(durationMs: Long, startMs: Long, maxMs: Long = AvatarBounds.VIDEO_MAX_MS.toLong()): Pair<Long, Long> {
    val length = min(durationMs, maxMs).coerceAtLeast(0L)
    val start = startMs.coerceIn(0L, max(0L, durationMs - length))
    return start to length
}

/** The crop square as Media3 `Crop` coordinates: normalised device
 *  coordinates `left, right, bottom, top`, each in -1..1, y up. */
public fun cropNdc(sourceWidth: Int, sourceHeight: Int, square: CropSquare): FloatArray {
    val w = sourceWidth.coerceAtLeast(1).toFloat()
    val h = sourceHeight.coerceAtLeast(1).toFloat()
    val left = square.left / w * 2f - 1f
    val right = (square.left + square.side) / w * 2f - 1f
    val top = 1f - square.top / h * 2f
    val bottom = 1f - (square.top + square.side) / h * 2f
    return floatArrayOf(left, right, bottom, top)
}
