// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** UX spec `docs/ux/media-messages.md` §10 — sizing and thumb budget rules. */
class VisualMediaTest {

    @Test
    fun fitWithinScalesDownProportionallyAndNeverUp() {
        assertEquals(PixelSize(4096, 3072), fitWithin(8000, 6000, 4096))
        assertEquals(PixelSize(2048, 4096), fitWithin(3000, 6000, 4096))
        assertEquals(PixelSize(1200, 900), fitWithin(1200, 900, 4096))
        assertEquals(PixelSize(320, 240), fitWithin(4032, 3024, THUMB_LONGEST_SIDE))
        assertEquals(PixelSize(4096, 4096), fitWithin(4096, 4096, 4096))
    }

    @Test
    fun fitWithinKeepsExtremeAspectsAtLeastOnePixel() {
        val s = fitWithin(100_000, 10, 320)!!
        assertEquals(320, s.width)
        assertEquals(1, s.height)
    }

    @Test
    fun fitWithinRefusesNonPositiveInput() {
        assertNull(fitWithin(0, 10, 320))
        assertNull(fitWithin(10, -1, 320))
        assertNull(fitWithin(10, 10, 0))
    }

    @Test
    fun budgetReturnsTheFirstQualityUnderTarget() {
        val tried = ArrayList<Int>()
        val out = compressUnderBudget(target = 100, hardCap = 1000) { q ->
            tried += q
            ByteArray(if (q >= 65) 150 else 90)
        }
        assertEquals(90, out!!.size)
        assertEquals(listOf(85, 75, 65, 50), tried)
    }

    @Test
    fun budgetFallsBackToTheSmallestUnderTheHardCap() {
        val out = compressUnderBudget(target = 100, hardCap = 500, qualities = listOf(80, 40)) { q ->
            ByteArray(if (q == 80) 450 else 300)
        }
        assertEquals(300, out!!.size)
    }

    @Test
    fun budgetGivesUpAboveTheHardCap() {
        assertNull(compressUnderBudget(target = 100, hardCap = 200) { ByteArray(500) })
    }

    @Test
    fun budgetSkipsThrowingAndEmptyEncodes() {
        val good = byteArrayOf(1, 2, 3)
        val out = compressUnderBudget(target = 10, hardCap = 20, qualities = listOf(90, 80, 70)) { q ->
            when (q) {
                90 -> throw IllegalStateException("encoder")
                80 -> ByteArray(0)
                else -> good
            }
        }
        assertArrayEquals(good, out)
        assertNull(compressUnderBudget { null })
    }

    @Test
    fun bubbleUsesRefDimensionsWithinTheBox() {
        val landscape = mediaBubbleBox(4000, 3000, null, null)
        assertEquals(260f, landscape.widthDp, 0.01f)
        assertEquals(195f, landscape.heightDp, 0.01f)
        val portrait = mediaBubbleBox(3000, 4000, 240, 320)
        // The ref's own dimensions win over the thumb's.
        assertEquals(260f, portrait.widthDp, 0.01f)
        assertEquals(260f * 4 / 3, portrait.heightDp, 0.01f)
    }

    @Test
    fun tallContentIsCappedByHeight() {
        val box = mediaBubbleBox(1080, 1920, null, null)
        assertEquals(360f, box.heightDp, 0.01f)
        assertEquals(360f * 1080 / 1920, box.widthDp, 0.01f)
        assertTrue(box.widthDp <= MEDIA_BUBBLE_MAX_WIDTH_DP)
    }

    @Test
    fun bubbleFallsBackToThumbAspectThenFourByThree() {
        val fromThumb = mediaBubbleBox(null, null, 320, 320)
        assertEquals(260f, fromThumb.widthDp, 0.01f)
        assertEquals(260f, fromThumb.heightDp, 0.01f)
        val placeholder = mediaBubbleBox(null, 0, null, null)
        assertEquals(260f, placeholder.widthDp, 0.01f)
        assertEquals(195f, placeholder.heightDp, 0.01f)
    }

    @Test
    fun extremeAspectsNeverCollapseTheBubble() {
        val sliver = mediaBubbleBox(4096, 1, null, null)
        assertEquals(MEDIA_BUBBLE_MIN_SIDE_DP, sliver.heightDp, 0.01f)
        val needle = mediaBubbleBox(1, 4096, null, null)
        assertEquals(MEDIA_BUBBLE_MIN_SIDE_DP, needle.widthDp, 0.01f)
        assertEquals(360f, needle.heightDp, 0.01f)
    }

    @Test
    fun onlyQuarterTurnsSwapSides() {
        assertTrue(rotationSwapsSides(90))
        assertTrue(rotationSwapsSides(270))
        assertTrue(rotationSwapsSides(-90))
        assertFalse(rotationSwapsSides(0))
        assertFalse(rotationSwapsSides(180))
        assertFalse(rotationSwapsSides(360))
    }
}
