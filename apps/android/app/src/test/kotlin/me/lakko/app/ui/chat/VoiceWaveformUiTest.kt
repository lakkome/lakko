// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.activity.ComponentActivity
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.unit.dp
import me.lakko.app.ui.captureRootPixels
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.GraphicsMode

/**
 * ADR-0047 §1 — the voice bubble's waveform draws BARS, of the heights the
 * sender measured.
 *
 * A person on the deployed build reported the waveform as "a straight line"
 * instead of a wave. Two capture defects caused it (`f85e0721`: the first
 * `getMaxAmplitude` reading covered the microphone's startup transient and
 * pushed every real sample to the floor, and an all-silent fold returned a row
 * of zeros that drew FLATTER than the absent-waveform placeholder). Both
 * capture halves are pinned in `WaveformAndPlaybackTest`. What nothing pinned is
 * the DRAWING: whether amplitude bytes that differ reach the canvas as bars that
 * differ. The bars are a `Canvas`, so nothing in the semantics tree says; this
 * reads the pixels.
 *
 * The measurement is one function over a captured bitmap — the painted height of
 * each column — and it runs over two inputs whose verdicts must be opposite:
 * real amplitudes must produce columns of several different heights, and the
 * absent-waveform placeholder must produce columns that are all one height. The
 * second is the flat line the person saw, asserted AS a flat line, so the first
 * assertion cannot pass on a measurement too coarse to tell the two apart.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class VoiceWaveformUiTest {

    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    /**
     * Render [waveform] through the production [WaveformSeekBar] at the
     * production bar count and return the drawn height of each bar, left to
     * right.
     *
     * White bars on a black backdrop, so "painted" is a threshold on one channel
     * rather than a guess about the window background. A bar's height is the
     * TALLEST of its columns rather than each column measured on its own: a bar
     * is 2-3 pixels wide at the spec's 170dp over 40 bars and is drawn fully
     * rounded, so its outer columns are partly covered and read shorter than the
     * bar is. Reading those as heights of their own reported three heights for a
     * waveform that is one flat line — antialiasing, not amplitude.
     */
    private fun barHeights(waveform: ByteArray?): List<Int> {
        compose.setContent {
            Box(Modifier.background(Color.Black)) {
                WaveformSeekBar(
                    waveform = waveform,
                    progress = 0f,
                    playedColor = Color.White,
                    idleColor = Color.White,
                    label = LABEL,
                    modifier = Modifier.width(170.dp).height(26.dp),
                    onSeek = {},
                )
            }
        }
        val bounds = compose.onNodeWithContentDescription(LABEL).fetchSemanticsNode().boundsInWindow
        val pixels = compose.captureRootPixels()
        val left = bounds.left.toInt()
        val top = bounds.top.toInt()
        val width = bounds.width.toInt()
        val height = bounds.height.toInt()
        val columns = (0 until width).map { x ->
            (0 until height).count { y -> pixels.redAt(left + x, top + y) > 127 }
        }
        // The same slot arithmetic the canvas draws with: one bar per slot,
        // `BARS` slots across the whole width.
        return (0 until BARS).map { bar ->
            val from = (bar * width) / BARS
            val to = ((bar + 1) * width) / BARS
            columns.subList(from, to).maxOrNull() ?: 0
        }
    }

    @Test
    fun realAmplitudesDrawBarsOfDifferingHeights() {
        // A loud-quiet-loud series, the shape of ordinary speech: the loudest
        // byte normalises to the full 26dp and the quietest to a sliver.
        val waveform = ByteArray(BARS) { i -> if (i % 4 == 0) -1 else 16 }
        val heights = barHeights(waveform)

        assertTrue("nothing was painted at all: $heights", heights.all { it > 0 })
        val distinct = heights.distinct()
        assertTrue(
            "every bar is the same height, so this is a line and not a wave: $distinct",
            distinct.size > 1,
        )
        // Not merely "different" — different by the amount the amplitudes say. A
        // one-pixel wobble would satisfy `distinct.size > 1` and still look like
        // the line the person reported.
        val tallest = heights.max()
        val shortest = heights.min()
        assertTrue(
            "tallest bar $tallest is not meaningfully taller than shortest $shortest",
            tallest >= shortest * 3,
        )
    }

    @Test
    fun theAbsentWaveformPlaceholderIsTheFlatLineThisTestWouldFail() {
        // The control, and the reason the assertion above discriminates: the
        // placeholder, every bar of which is 0.15 of the height, IS a flat line,
        // and the same measurement says so. If this ever reports several
        // heights, the measurement has stopped being able to see the defect and
        // the test above has stopped meaning anything.
        val heights = barHeights(null)

        assertTrue("nothing was painted at all", heights.all { it > 0 })
        assertEquals(
            "the placeholder is supposed to be one flat height: ${heights.distinct()}",
            1,
            heights.distinct().size,
        )
    }

    private companion object {
        const val LABEL = "Voice message position"

        /** [WaveformSeekBar]'s own default bar count. */
        const val BARS = 40
    }
}
