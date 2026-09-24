// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class WaveformAndPlaybackTest {

    private fun ByteArray.unsigned(): List<Int> = map { it.toInt() and 0xFF }

    @Test
    fun emptySeriesIsAbsent() {
        assertNull(downsampleWaveform(emptyList()))
    }

    @Test
    fun longSeriesFoldsToAtMost128Bytes() {
        val levels = List(9_000) { i -> (i % 50) / 50.0 }
        val out = downsampleWaveform(levels)!!
        assertEquals(MAX_WAVEFORM_BYTES, out.size)
    }

    @Test
    fun shortSeriesKeepsOneBytePerSample() {
        assertEquals(3, downsampleWaveform(listOf(0.1, 0.2, 0.4))!!.size)
    }

    @Test
    fun loudestBucketIsNormalisedTo255() {
        val out = downsampleWaveform(listOf(0.1, 0.2, 0.4))!!.unsigned()
        assertEquals(listOf(64, 128, 255), out)
    }

    @Test
    fun peakNotMeanSurvivesTheFold() {
        // One loud sample among silence in a single bucket must stay loud.
        val levels = List(256) { i -> if (i == 5) 1.0 else 0.0 }
        val out = downsampleWaveform(levels, buckets = 2)!!.unsigned()
        assertEquals(listOf(255, 0), out)
    }

    @Test
    fun anAllSilentTakeIsAbsentRatherThanARowOfZeros() {
        // A row of zeros floors at 0.08 in `waveformBars` while an ABSENT
        // waveform draws the 0.15 placeholder, so returning zeros made "we
        // measured nothing" draw FLATTER than "we have no waveform" — the
        // dotted line a silent capture put in the bubble. Nothing to draw
        // either way; say so in the way that draws the better placeholder.
        assertNull(downsampleWaveform(listOf(0.0, Double.NaN, -1.0, Double.POSITIVE_INFINITY)))
        assertNull(downsampleWaveform(List(64) { 0.0 }))
    }

    @Test
    fun theAbsentPlaceholderStandsTallerThanAFlooredZeroRow() {
        // Why the rule above is the right way round, asserted rather than
        // argued: this is the whole difference between the two placeholders.
        val absent = waveformBars(null, 8)
        val zeros = waveformBars(ByteArray(8), 8)
        assertTrue(absent.all { it > zeros.max() })
    }

    @Test
    fun oneSilentSampleDoesNotSilenceATakeThatHasSound() {
        // Only an ENTIRELY silent take is absent: a real take with a quiet
        // stretch still has a loudest bucket, so it still has a waveform.
        val out = downsampleWaveform(listOf(0.0, 0.0, 0.4))!!.unsigned()
        assertEquals(listOf(0, 0, 255), out)
    }

    @Test
    fun barsResampleAndKeepAFloor() {
        val bars = waveformBars(byteArrayOf(0, 255.toByte()), bars = 4)
        assertEquals(listOf(0.08f, 0.08f, 1f, 1f), bars)
        assertEquals(List(3) { 0.15f }, waveformBars(null, 3))
    }

    @Test
    fun oneOutsizedFirstSampleFloorsEverythingAfterIt() {
        // The mechanism behind the drained accumulator in
        // `VoiceRecorder.start`. `getMaxAmplitude` reports the peak SINCE THE
        // PREVIOUS CALL, so an undrained first reading covers an unbounded
        // window — the microphone's startup transient, the gain ramp, the tap
        // that began the take — and normalisation is by the LOUDEST bucket,
        // so that one artifact pushes every real sample to the floor and the
        // bubble draws a dotted line.
        //
        // This asserts the FOLD's behaviour, not the drain: the drain is one
        // statement against a live `MediaRecorder` with no seam to fake (see
        // the report). Keeping it means the reason the drain exists survives
        // even if someone removes the drain.
        val speech = List(16) { 0.25 }
        val withTransient = downsampleWaveform(listOf(1.0) + speech, buckets = 17)!!.unsigned()
        assertEquals(255, withTransient.first())
        assertTrue(withTransient.drop(1).all { it <= 64 })

        // Drained, the same speech is a full-height waveform.
        val drained = downsampleWaveform(speech, buckets = 16)!!.unsigned()
        assertTrue(drained.all { it == 255 })
    }

    @Test
    fun recorderAmplitudeMapsToLevel() {
        assertEquals(0.0, levelFromMaxAmplitude(-5), 0.0)
        assertEquals(1.0, levelFromMaxAmplitude(40_000), 0.0)
        assertEquals(0.5, levelFromMaxAmplitude(MAX_RECORDER_AMPLITUDE / 2), 0.001)
    }

    @Test
    fun autoContinueStaysInTheSameSendersConsecutiveRun() {
        val items = listOf(
            VoiceQueueItem("a", isVoice = true, senderKey = "alice", played = false),
            VoiceQueueItem("b", isVoice = true, senderKey = "alice", played = true),
            VoiceQueueItem("c", isVoice = true, senderKey = "alice", played = false),
            VoiceQueueItem("d", isVoice = true, senderKey = "bob", played = false),
        )
        assertEquals("c", nextVoiceToPlay(items, "a"))
        assertNull(nextVoiceToPlay(items, "c"))
    }

    @Test
    fun autoContinueStopsAtANonVoiceRowAndForUnknownSenders() {
        val items = listOf(
            VoiceQueueItem("a", isVoice = true, senderKey = "alice", played = false),
            VoiceQueueItem("t", isVoice = false, senderKey = "alice", played = true),
            VoiceQueueItem("c", isVoice = true, senderKey = "alice", played = false),
        )
        assertNull(nextVoiceToPlay(items, "a"))
        val unknown = listOf(
            VoiceQueueItem("a", isVoice = true, senderKey = null, played = false),
            VoiceQueueItem("b", isVoice = true, senderKey = null, played = false),
        )
        assertNull(nextVoiceToPlay(unknown, "a"))
        assertNull(nextVoiceToPlay(items, "missing"))
    }

    @Test
    fun speedCyclesAndLabels() {
        assertEquals(1.5f, nextPlaybackRate(1f))
        assertEquals(2f, nextPlaybackRate(1.5f))
        assertEquals(1f, nextPlaybackRate(2f))
        assertEquals("1x", rateLabel(1f))
        assertEquals("1.5x", rateLabel(1.5f))
    }

    /**
     * §2 — the speed control appears only while this message is the one
     * playing, and the row's geometry may not change at that moment, so the row
     * lays out [widestRateLabel] invisibly whether the control is there or not.
     * The reservation is only as good as the claim that no label can be wider
     * than it, so that is what is asserted — over [PLAYBACK_RATES] rather than
     * over the three labels of today, so adding a rate cannot leave a stale
     * reservation behind.
     */
    @Test
    fun theReservedRateLabelIsWiderThanEveryLabelTheControlCanShow() {
        val reserved = widestRateLabel()
        assertEquals("1.5x", reserved)
        assertTrue(PLAYBACK_RATES.isNotEmpty())
        for (rate in PLAYBACK_RATES) {
            val label = rateLabel(rate)
            assertTrue(
                "reserved '$reserved' is narrower than '$label'",
                label.length <= reserved.length,
            )
        }
        // And the reservation is one of them, not an invented string: a twin
        // that renders text the control never shows would reserve the wrong
        // glyphs.
        assertTrue(PLAYBACK_RATES.map(::rateLabel).contains(reserved))
    }

    /**
     * §2 — the clock shows the total before playback and the elapsed time
     * during it, so [voiceClockReservation] reserves the total's rendering.
     * The claim that keeps the bubble from resizing on the first frame of
     * playback is that no elapsed value can render wider than that, including
     * the minute rollovers where a character is added.
     */
    @Test
    fun theReservedClockIsWiderThanEveryElapsedValueItWillShow() {
        // Around each rollover, a cap and a sub-second take: the places where
        // the elapsed rendering gains a character or the total rounds up.
        for (durationMs in listOf(0L, 400L, 1_000L, 59_999L, 60_000L, 599_000L, 600_001L, 15 * 60_000L)) {
            val reserved = voiceClockReservation(durationMs)
            var positionMs = 0L
            while (positionMs <= durationMs) {
                assertTrue(
                    "elapsed '${formatClock(positionMs)}' is wider than reserved '$reserved'" +
                        " for a ${durationMs}ms message",
                    formatClock(positionMs).length <= reserved.length,
                )
                positionMs += 997L
            }
            assertTrue(formatClock(durationMs).length <= reserved.length)
        }
        // The ten-minute case the reservation exists for: without it the clock
        // would go from five characters to three on the first frame and the
        // bubble would shrink under the eye watching it.
        assertEquals("10:00", voiceClockReservation(600_000L))
        assertEquals("0:01", formatClock(1_000L))
    }

    @Test
    fun onlyOnePlayerAtATime() {
        val paused = ArrayList<String>()
        val slot = ExclusivePlayback()
        slot.claim("one") { paused += "one" }
        slot.claim("two") { paused += "two" }
        assertEquals(listOf("one"), paused)
        assertEquals("two", slot.current())
        // Re-claiming the current holder pauses nothing.
        slot.claim("two") { paused += "two-again" }
        assertEquals(listOf("one"), paused)
        // A stale release does not free somebody else's slot.
        slot.release("one")
        assertEquals("two", slot.current())
        slot.stopAll()
        assertEquals(listOf("one", "two-again"), paused)
        assertNull(slot.current())
    }
}
