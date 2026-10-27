// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** docs/ux/profile-screen.md §2.2 — crop math, the JPEG size loop and the
 *  10 s window. */
class AvatarCropTest {
    @Test
    fun at_one_x_the_short_side_fills_the_circle_centred() {
        val sq = cropSquare(4000, 3000, viewport = 300f, position = CropPosition())
        assertEquals(CropSquare(500, 0, 3000), sq)
    }

    @Test
    fun zoom_is_clamped_to_one_to_four() {
        assertEquals(1f, clampCrop(100, 100, 50f, CropPosition(zoom = 0.2f)).zoom)
        assertEquals(4f, clampCrop(100, 100, 50f, CropPosition(zoom = 9f)).zoom)
        val sq = cropSquare(1000, 1000, 100f, CropPosition(zoom = 4f))
        assertEquals(250, sq.side)
        assertEquals(375, sq.left)
    }

    @Test
    fun a_drag_never_uncovers_the_circle() {
        // A landscape picture can move sideways by half its overhang only.
        val moved = moveCrop(2000, 1000, 100f, CropPosition(), dx = 10_000f, dy = 10_000f)
        assertEquals(50f, moved.offsetX)
        assertEquals(0f, moved.offsetY)
        val sq = cropSquare(2000, 1000, 100f, moved)
        // Moving the picture right shows its left edge.
        assertEquals(0, sq.left)
        assertEquals(1000, sq.side)
    }

    @Test
    fun the_square_always_lies_inside_the_source() {
        for (zoom in listOf(1f, 1.5f, 2.7f, 4f)) {
            for (dx in listOf(-1e6f, -37f, 0f, 61f, 1e6f)) {
                val p = moveCrop(1234, 777, 333f, CropPosition(zoom = zoom), dx, -dx)
                val sq = cropSquare(1234, 777, 333f, p)
                assertTrue(sq.left >= 0 && sq.top >= 0)
                assertTrue(sq.left + sq.side <= 1234 && sq.top + sq.side <= 777)
            }
        }
    }

    @Test
    fun zooming_keeps_the_offset_with_the_picture() {
        val p = moveCrop(2000, 1000, 100f, CropPosition(offsetX = 20f), 0f, 0f, zoomBy = 2f)
        assertEquals(2f, p.zoom)
        assertEquals(40f, p.offsetX)
    }

    @Test
    fun output_side_is_capped() {
        assertEquals(640, outputSide(3000, 640))
        assertEquals(200, outputSide(200, 640))
        assertEquals(480, outputSide(1000, 480))
    }

    @Test
    fun jpeg_loop_lowers_quality_until_it_fits() {
        val tried = mutableListOf<Int>()
        val bytes = encodeUnderBudget(1000) { q ->
            tried += q
            ByteArray(q * 20)
        }
        assertEquals(listOf(90, 82, 74, 66, 58, 50), tried)
        assertEquals(1000, bytes?.size)
        assertNull(encodeUnderBudget(10) { ByteArray(100) })
    }

    @Test
    fun video_window_is_at_most_ten_seconds_and_fits() {
        assertEquals(0L to 4000L, videoWindow(4000, 2000))
        assertEquals(5000L to 10_000L, videoWindow(15_000, 5000))
        assertEquals(5000L to 10_000L, videoWindow(15_000, 9000))
        assertEquals(0L to 10_000L, videoWindow(30_000, -5))
    }

    @Test
    fun crop_ndc_maps_the_square_to_media3_coordinates() {
        val ndc = cropNdc(200, 100, CropSquare(50, 0, 100))
        assertArrayEquals(floatArrayOf(-0.5f, 0.5f, -1f, 1f), ndc, 1e-6f)
    }
}
