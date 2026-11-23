// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

/**
 * `docs/ux/media-messages.md` section 3, "Timeline bubble" (amended
 * 2026-11-22) — what sits on and beside a video-note circle.
 *
 * Seen on a real Pixel and a real iPhone: the duration, the time with its
 * ticks and the `Aa` control stood under the circle on loose lines and read as
 * text floating under a video. The note is now drawn like a photo: the
 * duration on a pill inside the circle at the bottom leading corner, the time
 * and ticks on a pill at the bottom trailing corner, and `Aa` beside the
 * circle on the side away from the screen edge. The same numbers are in the
 * web (`media/video-note-layout.ts`) and iOS (`VideoNoteLayout.swift`)
 * bubbles.
 */
public object VideoNoteLayout {
    /** Height of the pills' bottom edge above the circle's bottom, as a
     *  fraction of the diameter. */
    public const val PILL_BOTTOM: Float = 0.14f

    /** Distance of the pills' outer edge from the circle's side, as a fraction
     *  of the diameter. */
    public const val PILL_INSET: Float = 0.16f

    /** Gap between the circle and the `Aa` control beside it, in dp. */
    public const val ASIDE_GAP_DP: Int = 6

    /** Whether a pill's outer bottom corner, placed by the two fractions, lies
     *  inside a circle of [diameter]. */
    public fun pillCornerInside(diameter: Float): Boolean {
        val r = diameter / 2f
        val dx = r - PILL_INSET * diameter
        val dy = r - PILL_BOTTOM * diameter
        return dx * dx + dy * dy < r * r
    }

    /** `Aa` sits on the side away from the screen edge: after the circle on an
     *  incoming note, before it on an own note. */
    public fun asideFirst(isOwn: Boolean): Boolean = isOwn

    /** What the duration pill says. */
    public sealed interface DurationLabel {
        public data class Elapsed(val ms: Long) : DurationLabel
        public data class Total(val ms: Long) : DurationLabel
    }

    /** The elapsed time while the note is the one playing with sound, the
     *  total otherwise, or nothing without a duration. */
    public fun durationLabel(playingWithSound: Boolean, progress: Float, durationMs: Long?): DurationLabel? {
        if (durationMs == null) return null
        if (playingWithSound) {
            val fraction = if (progress.isFinite()) progress.coerceIn(0f, 1f) else 0f
            return DurationLabel.Elapsed((durationMs * fraction).toLong())
        }
        return DurationLabel.Total(durationMs)
    }
}
