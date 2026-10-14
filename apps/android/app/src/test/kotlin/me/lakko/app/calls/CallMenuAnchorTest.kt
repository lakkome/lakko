// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Test

class CallMenuAnchorTest {
    private val gap = CALL_MENU_EDGE_GAP_DP

    @Test
    fun `a menu with room on both sides is centred on its button`() {
        assertEquals(150f, anchoredMenuLeft(buttonCenterX = 200f, menuWidth = 100f, areaWidth = 400f, edgeGap = gap))
    }

    @Test
    fun `a menu over a button near the left edge keeps the gap from the left edge`() {
        assertEquals(gap, anchoredMenuLeft(buttonCenterX = 30f, menuWidth = 200f, areaWidth = 400f, edgeGap = gap))
    }

    @Test
    fun `a menu over a button near the right edge keeps the gap from the right edge`() {
        // The End button sits at the right end of the bar.
        val left = anchoredMenuLeft(buttonCenterX = 370f, menuWidth = 200f, areaWidth = 400f, edgeGap = gap)
        assertEquals(400f - 200f - gap, left)
    }

    @Test
    fun `a menu wider than the screen keeps its left edge on the screen`() {
        assertEquals(gap, anchoredMenuLeft(buttonCenterX = 200f, menuWidth = 500f, areaWidth = 400f, edgeGap = gap))
    }
}
