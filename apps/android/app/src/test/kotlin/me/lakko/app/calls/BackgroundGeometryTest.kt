// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The compositor's coordinate maps (`BackgroundGeometry.kt`): the model
 * pass and the blend agree on where each pixel of the picture is, for every
 * frame rotation, and the model sees the picture upright with its top row
 * first.
 */
class BackgroundGeometryTest {

    /**
     * What the Rust core's `backgroundModelInputRect` answers for the two
     * shapes a 1280 x 720 camera gives (`lakko-segment::render::model_input_rect`;
     * the host JVM cannot call it, so its values are written here).
     */
    private fun coreRect(uprightW: Int, uprightH: Int): ModelFit = when (uprightW to uprightH) {
        1280 to 720 -> ModelFit(0f, 0f, 256f, 144f)
        720 to 1280 -> ModelFit(87.5f, 0f, 81f, 144f)
        else -> error("no core value written for $uprightW x $uprightH")
    }

    private fun assertPoint(expected: Pair<Float, Float>, actual: Pair<Float, Float>) {
        assertEquals(expected.first, actual.first, 1e-4f)
        assertEquals(expected.second, actual.second, 1e-4f)
    }

    @Test
    fun `a landscape frame fills the model input`() {
        val fit = coreRect(1280, 720)
        // The model's first row is the picture's top: buffer y = 1.
        val toBuffer = BackgroundGeometry.modelToBuffer(0, fit, 256, 144)
        assertPoint(0f to 1f, toBuffer.apply(0f, 0f))
        assertPoint(1f to 0f, toBuffer.apply(1f, 1f))
    }

    @Test
    fun `a phone held upright is turned upright before it is fitted`() {
        val (w, h) = BackgroundGeometry.uprightSize(1280, 720, 270)
        assertEquals(720 to 1280, w to h)
        // The model's top-left inside the picture is the upright top-left.
        val fit = coreRect(w, h)
        val toBuffer = BackgroundGeometry.modelToBuffer(270, fit, 256, 144)
        val corner = toBuffer.apply(fit.offsetX / 256f, 0f)
        assertPoint(BackgroundGeometry.uprightToBuffer(270).apply(0f, 1f), corner)
    }

    @Test
    fun `the blend reads the mask where the model pass wrote it, for every rotation`() {
        for (rotation in listOf(0, 90, 180, 270)) {
            val (w, h) = BackgroundGeometry.uprightSize(1280, 720, rotation)
            val fit = coreRect(w, h)
            val toBuffer = BackgroundGeometry.modelToBuffer(rotation, fit, 256, 144)
            val toMask = BackgroundGeometry.bufferToMask(rotation, fit, 256, 144)
            for ((x, y) in listOf(0.5f to 0.5f, 0.4f to 0.3f, 0.6f to 0.9f)) {
                val b = toBuffer.apply(x, y)
                assertPoint(x to y, toMask.apply(b.first, b.second))
            }
            // Every buffer point lands inside the picture's part of the model input.
            for ((bx, by) in listOf(0f to 0f, 1f to 1f, 0f to 1f, 1f to 0f)) {
                val (mx, my) = toMask.apply(bx, by)
                assertEquals(true, mx >= fit.offsetX / 256f - 1e-4f && mx <= (fit.offsetX + fit.width) / 256f + 1e-4f)
                assertEquals(true, my >= fit.offsetY / 144f - 1e-4f && my <= (fit.offsetY + fit.height) / 144f + 1e-4f)
            }
        }
    }

    @Test
    fun `the upright turn matches what renderers sample`() {
        // A 90-degree frame: the upright top-left corner (0, 1) is sampled at
        // T(0.5) R(90) T(-0.5) of it, as VideoFrameDrawer builds it.
        val toBuffer = BackgroundGeometry.uprightToBuffer(90)
        assertPoint(0f to 0f, toBuffer.apply(0f, 1f))
        val back = BackgroundGeometry.bufferToUpright(90)
        assertPoint(0f to 1f, back.apply(0f, 0f))
    }

    @Test
    fun `a picture is cropped by its cover rectangle with its top row up`() {
        // A 2000 x 1000 picture behind a 1280 x 720 frame: the cover
        // rectangle the core gives is x 111.1, y 0, 1777.8 x 1000.
        val cover = floatArrayOf(111.1f, 0f, 1777.8f, 1000f)
        val toPicture = BackgroundGeometry.bufferToPicture(0, cover, 2000, 1000)
        // The frame's top-left (buffer 0, 1) is the cover's top-left.
        assertPoint(111.1f / 2000f to 0f, toPicture.apply(0f, 1f))
        assertPoint((111.1f + 1777.8f) / 2000f to 1f, toPicture.apply(1f, 0f))
    }
}
