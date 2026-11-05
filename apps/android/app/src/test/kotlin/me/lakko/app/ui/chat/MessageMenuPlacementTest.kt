// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** Section 2 — the action card never covers the reaction row, wherever the
 *  bubble is (seen on a Pixel 2026-11-04 with a message near the bottom). */
class MessageMenuPlacementTest {

    private val height = 2000
    private val gap = 20
    private val margin = 20
    private val rowH = 100
    private val cardH = 600

    private fun place(top: Int, bottom: Int, row: Int = rowH, card: Int = cardH, h: Int = height) =
        messageMenuPlacement(top, bottom, row, card, h, gap, margin)

    private fun assertClear(p: MenuPlacement, row: Int = rowH, card: Int = cardH, h: Int = height) {
        val rowRange = p.rowY until p.rowY + row
        val cardRange = p.cardY until p.cardY + card
        assertTrue("row $rowRange and card $cardRange overlap", rowRange.last < cardRange.first || cardRange.last < rowRange.first)
        assertTrue(p.rowY >= margin && p.rowY + row <= h - margin)
        assertTrue(p.cardY >= margin && p.cardY + card <= h - margin)
    }

    @Test
    fun `the ordinary case is the row above the bubble and the card below`() {
        val p = place(top = 800, bottom = 900)
        assertEquals(MenuPlacement(rowY = 680, cardY = 920), p)
        assertClear(p)
    }

    @Test
    fun `a message near the bottom puts the card above the row, never over it`() {
        val p = place(top = 1700, bottom = 1800)
        assertEquals(1580, p.rowY)
        assertEquals(1580 - gap - cardH, p.cardY)
        assertClear(p)
    }

    @Test
    fun `a message near the top puts the row and the card below`() {
        val p = place(top = 40, bottom = 140)
        assertEquals(160, p.rowY)
        assertEquals(160 + rowH + gap, p.cardY)
        assertClear(p)
    }

    @Test
    fun `a tall bubble filling the screen gets one block that is still clear`() {
        val p = place(top = 100, bottom = 1900)
        assertClear(p)
    }

    @Test
    fun `every bubble position keeps the row and the card apart`() {
        for (top in 0..1900 step 25) {
            for (bubble in listOf(60, 300, 900)) {
                val bottom = (top + bubble).coerceAtMost(height)
                assertClear(place(top, bottom))
            }
        }
    }

    @Test
    fun `a missing surface takes its gap with it`() {
        val noCard = place(top = 1700, bottom = 1800, card = 0)
        assertEquals(1580, noCard.rowY)
        val noRow = place(top = 800, bottom = 900, row = 0)
        assertEquals(920, noRow.cardY)
    }

    @Test
    fun `the taller quick reaction bar is still kept clear of the card`() {
        // 44dp targets + 4dp padding top and bottom, at a Pixel's 2.625 density;
        // a card of eight 40dp rows.
        val row = (52 * 2.625).toInt()
        val card = (8 * 40 * 2.625).toInt()
        val h = 2400
        for (top in 0..2300 step 20) {
            for (bubble in listOf(120, 400, 1600)) {
                val bottom = (top + bubble).coerceAtMost(h)
                assertClear(place(top, bottom, row = row, card = card, h = h), row = row, card = card, h = h)
            }
        }
    }

    @Test
    fun `the widest reaction chip fits a phone bubble`() {
        // Padding, emoji, gap, three overlapping avatars, padding (dp).
        val widest = REACTION_CHIP_PADDING.value + 22f + REACTION_CHIP_INNER_GAP.value +
            (REACTION_CHIP_MAX_AVATARS * REACTION_AVATAR_SIZE.value -
                (REACTION_CHIP_MAX_AVATARS - 1) * REACTION_AVATAR_OVERLAP.value) +
            REACTION_CHIP_PADDING.value
        // A 360dp phone's bubble is at most 78% of the width.
        assertTrue("widest chip $widest dp", widest <= 360f * 0.78f)
        assertEquals(32f, REACTION_CHIP_HEIGHT.value)
    }
}
