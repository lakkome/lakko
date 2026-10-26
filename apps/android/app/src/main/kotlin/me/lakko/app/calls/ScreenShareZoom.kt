// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * Zoom and pan on a shared screen in the large place
 * (`docs/ux/screen-share.md` section 5.3): pinch to zoom, drag to pan, a
 * double tap back to whole. The picture never leaves its box: at a zoom of
 * `s` it may move by at most `(s - 1) / 2` of the box each way.
 *
 * Pure, so the bounds are unit-tested on the host JVM; the call screen feeds
 * it gestures and draws the answer.
 */
public data class ShareZoom(
    /** 1 is the whole picture. */
    val scale: Float = 1f,
    /** The picture's offset from the centre, in pixels. */
    val offsetX: Float = 0f,
    val offsetY: Float = 0f,
) {
    /** Whether the picture is shown whole. */
    public val isWhole: Boolean get() = scale <= 1f

    /**
     * One gesture step: zoom by [zoom] and move by ([panX], [panY]) pixels,
     * in a box of [width] by [height] pixels.
     */
    public fun step(zoom: Float, panX: Float, panY: Float, width: Float, height: Float): ShareZoom {
        val factor = if (zoom.isFinite() && zoom > 0f) zoom else 1f
        val nextScale = (scale * factor).coerceIn(1f, MAX_SHARE_ZOOM)
        if (nextScale <= 1f) return Whole
        val maxX = width.coerceAtLeast(0f) * (nextScale - 1f) / 2f
        val maxY = height.coerceAtLeast(0f) * (nextScale - 1f) / 2f
        return ShareZoom(
            scale = nextScale,
            offsetX = (offsetX + panX.finiteOrZero()).coerceIn(-maxX, maxX),
            offsetY = (offsetY + panY.finiteOrZero()).coerceIn(-maxY, maxY),
        )
    }

    public companion object {
        /** The whole picture, no offset. */
        public val Whole: ShareZoom = ShareZoom()
    }
}

/** The largest zoom on a shared screen. Chosen, not measured. */
public const val MAX_SHARE_ZOOM: Float = 5f

private fun Float.finiteOrZero(): Float = if (isFinite()) this else 0f
