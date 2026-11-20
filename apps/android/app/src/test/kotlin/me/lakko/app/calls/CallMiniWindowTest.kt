// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/calls.md` section 4.5.2 — the floating call window's size and
 * place, and section 4.5.1's `+N`, as the pure functions in
 * [miniWindowSize] and its neighbours decide them.
 */
class CallMiniWindowTest {

    @After
    fun forgetTheCorner() = MiniWindowPlacement.reset()

    // ----- size --------------------------------------------------------------

    @Test
    fun aPhoneWindowIsUpright34At36PercentOfTheWidth() {
        val size = miniWindowSize(screenWidthDp = 400f, screenHeightDp = 860f, screenShare = false)
        assertEquals(144f, size.width, 0.001f)
        assertEquals(192f, size.height, 0.001f)
    }

    @Test
    fun aNarrowPhoneWindowIsNeverUnder120() {
        val size = miniWindowSize(screenWidthDp = 300f, screenHeightDp = 640f, screenShare = false)
        assertEquals(120f, size.width, 0.001f)
        assertEquals(160f, size.height, 0.001f)
    }

    @Test
    fun aTabletWindowLies169At320() {
        val size = miniWindowSize(screenWidthDp = 800f, screenHeightDp = 1280f, screenShare = false)
        assertEquals(320f, size.width, 0.001f)
        assertEquals(180f, size.height, 0.001f)
    }

    @Test
    fun aPhoneInLandscapeIsStillAPhone() {
        // The narrow side decides, so turning a phone does not make it a tablet.
        val size = miniWindowSize(screenWidthDp = 860f, screenHeightDp = 400f, screenShare = false)
        assertEquals(860f * 0.36f, size.width, 0.001f)
        assertEquals(size.width * 4f / 3f, size.height, 0.001f)
    }

    @Test
    fun aShareIs169AtThePhoneWidth() {
        val size = miniWindowSize(screenWidthDp = 400f, screenHeightDp = 860f, screenShare = true)
        assertEquals(144f, size.width, 0.001f)
        assertEquals(81f, size.height, 0.001f)
    }

    @Test
    fun onlyAPhoneTucks() {
        assertTrue(miniWindowTuckAllowed(400f, 860f))
        assertFalse(miniWindowTuckAllowed(800f, 1280f))
    }

    // ----- corners -----------------------------------------------------------

    private val area = MiniWindowArea(left = 16f, top = 100f, right = 384f, bottom = 800f)
    private val size = MiniWindowSize(width = 144f, height = 192f)

    @Test
    fun itStartsTopRight() {
        assertEquals(MiniWindowCorner.TOP_END, MiniWindowCorner.INITIAL)
        assertEquals(MiniWindowCorner.TOP_END, MiniWindowPlacement.corner)
        val p = miniWindowCornerPosition(MiniWindowCorner.TOP_END, area, size)
        assertEquals(240f, p.x, 0.001f)
        assertEquals(100f, p.y, 0.001f)
    }

    @Test
    fun eachCornerSitsInsideTheArea() {
        assertEquals(MiniWindowPoint(16f, 100f), miniWindowCornerPosition(MiniWindowCorner.TOP_START, area, size))
        assertEquals(MiniWindowPoint(16f, 608f), miniWindowCornerPosition(MiniWindowCorner.BOTTOM_START, area, size))
        assertEquals(MiniWindowPoint(240f, 608f), miniWindowCornerPosition(MiniWindowCorner.BOTTOM_END, area, size))
    }

    @Test
    fun aBottomCornerRisesWithTheKeyboard() {
        // The keyboard shrinks the area from below; the window follows it.
        val withKeyboard = area.copy(bottom = 500f)
        val p = miniWindowCornerPosition(MiniWindowCorner.BOTTOM_END, withKeyboard, size)
        assertEquals(308f, p.y, 0.001f)
    }

    @Test
    fun anAreaTooSmallPinsTheWindowToItsTopLeft() {
        val tiny = MiniWindowArea(left = 16f, top = 100f, right = 100f, bottom = 150f)
        assertEquals(MiniWindowPoint(16f, 100f), miniWindowCornerPosition(MiniWindowCorner.BOTTOM_END, tiny, size))
    }

    // ----- release -----------------------------------------------------------

    @Test
    fun aSlowReleaseLandsInTheNearestCorner() {
        val rest = miniWindowRelease(
            position = MiniWindowPoint(30f, 600f),
            velocity = MiniWindowPoint(0f, 0f),
            area = area,
            size = size,
            tuckAllowed = true,
        )
        assertEquals(MiniWindowRest.InCorner(MiniWindowCorner.BOTTOM_START), rest)
    }

    @Test
    fun aFlickReachesTheCornerItWasThrownTowards() {
        // Let go in the top-left quarter but moving fast down and right: it
        // lands bottom right, where it would have come to rest.
        val rest = miniWindowRelease(
            position = MiniWindowPoint(30f, 150f),
            velocity = MiniWindowPoint(1000f, 2500f),
            area = area,
            size = size,
            tuckAllowed = true,
        )
        assertEquals(MiniWindowRest.InCorner(MiniWindowCorner.BOTTOM_END), rest)
    }

    @Test
    fun aFlickPastTheLeftEdgeTucksItAwayOnAPhone() {
        val rest = miniWindowRelease(
            position = MiniWindowPoint(20f, 300f),
            velocity = MiniWindowPoint(-3000f, 0f),
            area = area,
            size = size,
            tuckAllowed = true,
        )
        assertEquals(MiniWindowRest.Tucked(atStart = true), rest)
    }

    @Test
    fun aFlickPastTheRightEdgeTucksItAwayOnAPhone() {
        val rest = miniWindowRelease(
            position = MiniWindowPoint(230f, 300f),
            velocity = MiniWindowPoint(3000f, 0f),
            area = area,
            size = size,
            tuckAllowed = true,
        )
        assertEquals(MiniWindowRest.Tucked(atStart = false), rest)
    }

    @Test
    fun withoutTuckTheSameFlickLandsInACorner() {
        val rest = miniWindowRelease(
            position = MiniWindowPoint(20f, 300f),
            velocity = MiniWindowPoint(-3000f, 0f),
            area = area,
            size = size,
            tuckAllowed = false,
        )
        assertEquals(MiniWindowRest.InCorner(MiniWindowCorner.TOP_START), rest)
    }

    @Test
    fun touchingAnEdgeDoesNotTuck() {
        // Measured on the centre: a window dragged against the edge stays.
        val rest = miniWindowRelease(
            position = MiniWindowPoint(-40f, 300f),
            velocity = MiniWindowPoint(0f, 0f),
            area = area,
            size = size,
            tuckAllowed = true,
        )
        assertEquals(MiniWindowRest.InCorner(MiniWindowCorner.TOP_START), rest)
    }

    @Test
    fun aBrokenVelocityIsReadAsStill() {
        val rest = miniWindowRelease(
            position = MiniWindowPoint(230f, 600f),
            velocity = MiniWindowPoint(Float.NaN, Float.POSITIVE_INFINITY),
            area = area,
            size = size,
            tuckAllowed = true,
        )
        assertEquals(MiniWindowRest.InCorner(MiniWindowCorner.BOTTOM_END), rest)
    }

    @Test
    fun pullingTheStripBackInByHalfItsWidthShowsTheWindow() {
        assertTrue(miniWindowStripPulledOut(atStart = true, dragX = 14f, stripWidth = 28f))
        assertFalse(miniWindowStripPulledOut(atStart = true, dragX = 10f, stripWidth = 28f))
        assertFalse(miniWindowStripPulledOut(atStart = true, dragX = -40f, stripWidth = 28f))
        assertTrue(miniWindowStripPulledOut(atStart = false, dragX = -20f, stripWidth = 28f))
        assertFalse(miniWindowStripPulledOut(atStart = false, dragX = 20f, stripWidth = 28f))
    }

    // ----- inset and +N --------------------------------------------------------

    @Test
    fun theInsetSitsInTheBottomCornerAwayFromTheEdge() {
        assertTrue(miniWindowInsetAtStart(MiniWindowCorner.TOP_END))
        assertTrue(miniWindowInsetAtStart(MiniWindowCorner.BOTTOM_END))
        assertFalse(miniWindowInsetAtStart(MiniWindowCorner.TOP_START))
        assertFalse(miniWindowInsetAtStart(MiniWindowCorner.BOTTOM_START))
    }

    @Test
    fun plusNCountsEveryoneBeyondThePersonShownAndYou() {
        assertEquals(16, miniWindowPeopleBeyond(people = 18, shownIsLocal = false, groupCall = true))
        assertEquals(0, miniWindowPeopleBeyond(people = 2, shownIsLocal = false, groupCall = true))
        assertEquals(0, miniWindowPeopleBeyond(people = 1, shownIsLocal = true, groupCall = true))
        assertEquals(0, miniWindowPeopleBeyond(people = 5, shownIsLocal = true, groupCall = true))
        assertEquals(0, miniWindowPeopleBeyond(people = 2, shownIsLocal = false, groupCall = false))
    }

    @Test
    fun theWindowWidthIsAskedInPhysicalPixels() {
        assertEquals(396, miniWindowWidthPx(144f, 2.75f))
        assertEquals(144, miniWindowWidthPx(144f, Float.NaN))
        assertEquals(0, miniWindowWidthPx(-1f, 2f))
    }

    @Test
    fun theLandedCornerIsKeptForTheNextCall() {
        MiniWindowPlacement.corner = MiniWindowCorner.BOTTOM_START
        assertEquals(MiniWindowCorner.BOTTOM_START, MiniWindowPlacement.corner)
        MiniWindowPlacement.reset()
        assertEquals(MiniWindowCorner.TOP_END, MiniWindowPlacement.corner)
    }
}
