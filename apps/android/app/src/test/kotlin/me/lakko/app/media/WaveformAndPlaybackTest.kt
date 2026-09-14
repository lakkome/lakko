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
    fun silentAndNonFiniteSamplesAreZeros() {
        val out = downsampleWaveform(listOf(0.0, Double.NaN, -1.0, Double.POSITIVE_INFINITY))!!
        assertTrue(out.all { it.toInt() == 0 })
    }

    @Test
    fun barsResampleAndKeepAFloor() {
        val bars = waveformBars(byteArrayOf(0, 255.toByte()), bars = 4)
        assertEquals(listOf(0.08f, 0.08f, 1f, 1f), bars)
        assertEquals(List(3) { 0.15f }, waveformBars(null, 3))
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
