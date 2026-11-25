// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.chat.StickToBottom.Action
import me.lakko.app.chat.StickToBottom.Geometry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The timeline keeps its newest message in view when the space above the
 * composer shrinks (keyboard, reply / edit strip, pinned bar) or the last
 * bubble grows, and leaves a person reading older messages alone.
 */
class StickToBottomTest {

    /** 20 rows, the last one ending exactly at the end of a 1000 px viewport. */
    private val atBottom = Geometry(totalItems = 20, lastVisibleIndex = 19, lastVisibleEnd = 1000, viewportEnd = 1000)

    @Test
    fun theKeyboardOpeningScrollsTheNewestRowBackAboveTheComposer() {
        // The viewport shrank by 300 px and the list kept its first row: the
        // last row now ends 300 px below the visible end.
        val d = StickToBottom.decide(stuck = true, scrolling = false, geometry = atBottom.copy(viewportEnd = 700))
        assertEquals(Action.ScrollBy(300), d.action)
        assertTrue(d.stuck)
    }

    @Test
    fun aBarAppearingAboveTheListScrollsByItsHeight() {
        val d = StickToBottom.decide(stuck = true, scrolling = false, geometry = atBottom.copy(viewportEnd = 944))
        assertEquals(Action.ScrollBy(56), d.action)
    }

    @Test
    fun theLastBubbleGrowingAfterAnEditKeepsItsEndInView() {
        val d = StickToBottom.decide(stuck = true, scrolling = false, geometry = atBottom.copy(lastVisibleEnd = 1040))
        assertEquals(Action.ScrollBy(40), d.action)
    }

    @Test
    fun aLastRowPushedOutOfViewEntirelyIsJumpedTo() {
        val g = Geometry(totalItems = 20, lastVisibleIndex = 17, lastVisibleEnd = 700, viewportEnd = 700)
        val d = StickToBottom.decide(stuck = true, scrolling = false, geometry = g)
        assertEquals(Action.JumpToEnd, d.action)
        assertTrue(d.stuck)
    }

    @Test
    fun aPersonReadingOlderMessagesIsLeftWhereTheyAre() {
        val reading = Geometry(totalItems = 20, lastVisibleIndex = 9, lastVisibleEnd = 1000, viewportEnd = 700)
        val d = StickToBottom.decide(stuck = false, scrolling = false, geometry = reading)
        assertEquals(Action.None, d.action)
        assertFalse(d.stuck)
    }

    @Test
    fun scrollingAwayFromTheBottomReleasesIt() {
        val g = Geometry(totalItems = 20, lastVisibleIndex = 15, lastVisibleEnd = 1000, viewportEnd = 1000)
        val d = StickToBottom.decide(stuck = true, scrolling = true, geometry = g)
        assertEquals(Action.None, d.action)
        assertFalse(d.stuck)
    }

    @Test
    fun scrollingBackToTheBottomHoldsItAgain() {
        val d = StickToBottom.decide(stuck = false, scrolling = true, geometry = atBottom)
        assertEquals(Action.None, d.action)
        assertTrue(d.stuck)
    }

    @Test
    fun aRowEndingWithinTheToleranceCountsAsAtTheBottom() {
        assertTrue(atBottom.copy(lastVisibleEnd = 1001).atBottom)
        assertFalse(atBottom.copy(lastVisibleEnd = 1002).atBottom)
        assertTrue(Geometry(0, -1, 0, 1000).atBottom)
    }

    @Test
    fun theSpaceGrowingAtTheBottomNeedsNothing() {
        val d = StickToBottom.decide(stuck = true, scrolling = false, geometry = atBottom.copy(viewportEnd = 1300))
        assertEquals(Action.None, d.action)
        assertTrue(d.stuck)
    }
}
