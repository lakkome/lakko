// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlin.math.max
import kotlin.math.roundToInt

/**
 * The collapsed call's floating window — `docs/ux/calls.md` section 4.5 —
 * everything about it that is not Compose: its size, the corner it rests in,
 * where a fling lands, the tucked strip, and the `+N` beside the name.
 *
 * Where a window lands is a fact about one device and one hand (section
 * 4.5.2), so the placement is this client's own and not a shared rule; what
 * the window SHOWS comes from the shared rules (`call_bar`, `call_stage`,
 * `tile_video_quality`) and is not decided here.
 *
 * Every function here is pure and works in one unit the caller picks (dp or
 * px), so the host JVM suite covers all of it.
 */

/** One of the four corners the window rests in. */
public enum class MiniWindowCorner(public val top: Boolean, public val start: Boolean) {
    TOP_START(top = true, start = true),
    TOP_END(top = true, start = false),
    BOTTOM_START(top = false, start = true),
    BOTTOM_END(top = false, start = false),
    ;

    public companion object {
        /** Section 4.5.2: "It starts in the top right corner". */
        public val INITIAL: MiniWindowCorner = TOP_END

        public fun of(top: Boolean, start: Boolean): MiniWindowCorner = when {
            top && start -> TOP_START
            top -> TOP_END
            start -> BOTTOM_START
            else -> BOTTOM_END
        }
    }
}

/** A width and a height, in the caller's unit. */
public data class MiniWindowSize(val width: Float, val height: Float)

/** A position (the window's top-left), in the caller's unit. */
public data class MiniWindowPoint(val x: Float, val y: Float)

/**
 * The rectangle the window may occupy: the screen minus the system bars, the
 * software keyboard and the screen's own header, inset by the margin. The
 * window's top-left ranges over `[left, right - width] x [top, bottom - height]`.
 */
public data class MiniWindowArea(val left: Float, val top: Float, val right: Float, val bottom: Float)

/** The width under which a screen is a phone (section 4.5.2: "the narrow side
 *  of the screen under 600 points"). */
public const val MINI_WINDOW_PHONE_MAX_NARROW_SIDE_DP: Float = 600f

/** A phone window is this fraction of the screen width. */
public const val MINI_WINDOW_PHONE_WIDTH_FRACTION: Float = 0.36f

/** ... and never narrower than this. */
public const val MINI_WINDOW_PHONE_MIN_WIDTH_DP: Float = 120f

/** A tablet's window, and any lying window, is this wide. */
public const val MINI_WINDOW_WIDE_WIDTH_DP: Float = 320f

/** "16 points in from the safe edges". */
public const val MINI_WINDOW_MARGIN_DP: Float = 16f

/**
 * How far below the status bar the top corners sit so the window does not
 * cover a screen's own header (section 4.5.2: "below the screen's own
 * header"). The height of a Material top app bar; the app's screens use one.
 */
public const val MINI_WINDOW_HEADER_DP: Float = 64f

/**
 * How far above the bottom safe edge the bottom corners sit, so a window put
 * in a bottom corner covers neither the tab bar nor a chat's composer: the
 * height of a Material navigation bar. Chosen, not measured, and not in the
 * UX spec, which names only the system bars and the keyboard; seen on the
 * emulator without it, the window lay over the Chats and Feed tabs.
 */
public const val MINI_WINDOW_FOOTER_DP: Float = 80f

/** The width of the strip left at an edge when the window is tucked away. */
public const val MINI_WINDOW_TUCK_STRIP_WIDTH_DP: Float = 28f

/** The strip's height. */
public const val MINI_WINDOW_TUCK_STRIP_HEIGHT_DP: Float = 72f

/**
 * How far a fling carries the window before it would come to rest, as seconds
 * of its release velocity. A simple stand-in for friction; chosen, not
 * measured — long enough that a flick towards a corner reaches it, short
 * enough that a slow drag lands where it was let go.
 */
public const val MINI_WINDOW_FLING_PROJECTION_SECONDS: Float = 0.2f

/**
 * The window's size for a screen of [screenWidthDp] by [screenHeightDp]
 * (section 4.5.2's table):
 *
 *  - a phone (narrow side under 600): 3:4 upright, 36% of the screen width and
 *    at least 120 wide;
 *  - anything wider: 16:9 lying, 320 wide;
 *  - while the picture is a screen share: 16:9 at the width above.
 */
public fun miniWindowSize(screenWidthDp: Float, screenHeightDp: Float, screenShare: Boolean): MiniWindowSize {
    val phone = minOf(screenWidthDp, screenHeightDp) < MINI_WINDOW_PHONE_MAX_NARROW_SIDE_DP
    val width = if (phone) {
        max(screenWidthDp * MINI_WINDOW_PHONE_WIDTH_FRACTION, MINI_WINDOW_PHONE_MIN_WIDTH_DP)
    } else {
        MINI_WINDOW_WIDE_WIDTH_DP
    }
    val lying = !phone || screenShare
    val height = if (lying) width * 9f / 16f else width * 4f / 3f
    return MiniWindowSize(width = width, height = height)
}

/** Whether a fling past the side can tuck the window away: phones only
 *  (section 4.5.2: "On a phone, a flick past the left or right edge tucks it
 *  away"). */
public fun miniWindowTuckAllowed(screenWidthDp: Float, screenHeightDp: Float): Boolean =
    minOf(screenWidthDp, screenHeightDp) < MINI_WINDOW_PHONE_MAX_NARROW_SIDE_DP

/**
 * Where the window's top-left sits in [corner]. An area too small to hold the
 * window pins it to the area's top-left rather than placing it outside.
 */
public fun miniWindowCornerPosition(corner: MiniWindowCorner, area: MiniWindowArea, size: MiniWindowSize): MiniWindowPoint {
    val maxX = max(area.left, area.right - size.width)
    val maxY = max(area.top, area.bottom - size.height)
    return MiniWindowPoint(
        x = if (corner.start) area.left else maxX,
        y = if (corner.top) area.top else maxY,
    )
}

/** Where the window ends up after a release. */
public sealed class MiniWindowRest {
    /** In a corner. */
    public data class InCorner(val corner: MiniWindowCorner) : MiniWindowRest()

    /** Tucked past the left ([atStart]) or right edge of the screen. It
     *  comes back to the corner it came from when it is shown again
     *  (section 4.5.2), which the caller holds. */
    public data class Tucked(val atStart: Boolean) : MiniWindowRest()
}

/**
 * Where a window released at [position] moving at [velocity] (units per
 * second) comes to rest (section 4.5.2): the corner nearest to where it would
 * stop given how fast it was moving, so a flick towards a corner reaches it.
 *
 * On a phone ([tuckAllowed]), a window whose projected centre lies past the
 * left or right edge of the area is tucked to that edge instead. Measured on
 * the CENTRE, so a drag that merely touches an edge does not hide the call.
 *
 * The area is in screen coordinates, so "start" here is the left; a caller
 * laid out right to left mirrors the corner it gets back.
 *
 * Total: a non-finite velocity is read as zero, so a broken event never throws
 * the window off screen.
 */
public fun miniWindowRelease(
    position: MiniWindowPoint,
    velocity: MiniWindowPoint,
    area: MiniWindowArea,
    size: MiniWindowSize,
    tuckAllowed: Boolean,
    projectionSeconds: Float = MINI_WINDOW_FLING_PROJECTION_SECONDS,
): MiniWindowRest {
    val vx = if (velocity.x.isFinite()) velocity.x else 0f
    val vy = if (velocity.y.isFinite()) velocity.y else 0f
    val restX = position.x + vx * projectionSeconds
    val restY = position.y + vy * projectionSeconds
    val centreX = restX + size.width / 2f
    val centreY = restY + size.height / 2f
    val midX = (area.left + area.right) / 2f
    val midY = (area.top + area.bottom) / 2f
    val corner = MiniWindowCorner.of(top = centreY < midY, start = centreX < midX)
    if (tuckAllowed) {
        if (centreX < area.left) return MiniWindowRest.Tucked(atStart = true)
        if (centreX > area.right) return MiniWindowRest.Tucked(atStart = false)
    }
    return MiniWindowRest.InCorner(corner)
}

/**
 * Whether a drag of the tucked strip by [dragX] (positive = towards the end
 * edge) pulls the window back out: a pull away from the edge it is tucked at,
 * of at least half the strip's width.
 */
public fun miniWindowStripPulledOut(atStart: Boolean, dragX: Float, stripWidth: Float): Boolean {
    val inward = if (atStart) dragX else -dragX
    return inward >= stripWidth / 2f
}

/**
 * Where the own camera inset sits inside the window (section 4.5.1: "in the
 * bottom corner of the window away from the screen edge it sits on"): `true`
 * for the window's bottom-start corner, which is the one away from an
 * end-side window.
 */
public fun miniWindowInsetAtStart(corner: MiniWindowCorner): Boolean = !corner.start

/**
 * The `+N` beside the shown person's name in a group call (section 4.5.1:
 * "`+N` for everyone else beyond that person and you"): `+16` with 18 people.
 * Zero (nothing to print) for a one-to-one call, for a person alone in a call,
 * and when the picture already is you.
 *
 * @param people everyone in the call, this device's own person included.
 * @param shownIsLocal the picture is this device's own person.
 */
public fun miniWindowPeopleBeyond(people: Int, shownIsLocal: Boolean, groupCall: Boolean): Int {
    if (!groupCall || shownIsLocal) return 0
    return (people - 2).coerceAtLeast(0)
}

/**
 * Where the window will be found while the app runs (section 4.5.2: "The
 * corner it lands in is kept while the app runs and is where the next
 * collapsed call opens"). A process object, not persisted: a fresh launch
 * starts in the top right again.
 */
public object MiniWindowPlacement {
    @Volatile
    public var corner: MiniWindowCorner = MiniWindowCorner.INITIAL

    /** Forget the corner (tests). */
    public fun reset() {
        corner = MiniWindowCorner.INITIAL
    }
}

/**
 * The width of the window in physical pixels, as the shared quality rule
 * wants it (`tile_video_quality` measures in physical pixels).
 */
public fun miniWindowWidthPx(widthDp: Float, density: Float): Int {
    val ratio = if (density.isFinite() && density > 0f) density else 1f
    if (!widthDp.isFinite() || widthDp <= 0f) return 0
    return (widthDp * ratio).roundToInt()
}

/** The aspect ratio the system picture-in-picture window takes (section
 *  4.5.4): the window's own shape, as a width and a height. */
public data class PipAspect(val width: Int, val height: Int)

/** 3:4 upright for a person, 16:9 for a screen being shared. */
public fun pictureInPictureAspect(screenShare: Boolean): PipAspect =
    if (screenShare) PipAspect(16, 9) else PipAspect(3, 4)
