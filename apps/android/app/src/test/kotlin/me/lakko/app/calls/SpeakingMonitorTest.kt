// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Reading who is speaking (`docs/ux/group-call-grid.md` section 7.3): the
 * levels picked out of a statistics report, and what the monitor does with a
 * reading. The connection itself is not here; a host test has none.
 */
class SpeakingMonitorTest {

    // ---- The levels in a statistics report -----------------------------------

    private val tracks = mapOf("track-a" to "peer-a-id", "track-b" to "peer-b-id")

    @Test
    fun `a remote level is put on the person the track arrived from`() {
        val levels = audioLevelsFrom(
            listOf(
                RtcStat("inbound-rtp", mapOf("kind" to "audio", "trackIdentifier" to "track-a", "audioLevel" to 0.4)),
                RtcStat("inbound-rtp", mapOf("kind" to "audio", "trackIdentifier" to "track-b", "audioLevel" to 0.0)),
            ),
            tracks,
        )
        assertEquals(mapOf("peer-a-id" to 0.4, "peer-b-id" to 0.0), levels.remoteByStreamId)
        assertNull(levels.local)
    }

    @Test
    fun `this device's own level is the audio source's`() {
        val levels = audioLevelsFrom(
            listOf(
                RtcStat("media-source", mapOf("kind" to "audio", "audioLevel" to 0.25f)),
                RtcStat("media-source", mapOf("kind" to "video")),
            ),
            tracks,
        )
        assertEquals(0.25, levels.local!!, 1e-6)
        assertTrue(levels.remoteByStreamId.isEmpty())
    }

    @Test
    fun `what cannot be read is left out, never guessed`() {
        val levels = audioLevelsFrom(
            listOf(
                // A picture has no sound level.
                RtcStat("inbound-rtp", mapOf("kind" to "video", "trackIdentifier" to "track-a", "audioLevel" to 0.9)),
                // A track nobody recorded the arrival of.
                RtcStat("inbound-rtp", mapOf("kind" to "audio", "trackIdentifier" to "unknown", "audioLevel" to 0.9)),
                // No level in the entry.
                RtcStat("inbound-rtp", mapOf("kind" to "audio", "trackIdentifier" to "track-b")),
                // Not a number.
                RtcStat("inbound-rtp", mapOf("kind" to "audio", "trackIdentifier" to "track-a", "audioLevel" to Double.NaN)),
                // What this device sends is not what it hears.
                RtcStat("outbound-rtp", mapOf("kind" to "audio", "audioLevel" to 0.9)),
            ),
            tracks,
        )
        assertTrue(levels.remoteByStreamId.isEmpty())
        assertNull(levels.local)
    }

    @Test
    fun `two entries for one person keep the louder`() {
        val levels = audioLevelsFrom(
            listOf(
                RtcStat("inbound-rtp", mapOf("kind" to "audio", "trackIdentifier" to "track-a", "audioLevel" to 0.1)),
                RtcStat("inbound-rtp", mapOf("kind" to "audio", "trackIdentifier" to "track-a", "audioLevel" to 0.6)),
            ),
            mapOf("track-a" to "peer-a-id"),
        )
        assertEquals(mapOf("peer-a-id" to 0.6), levels.remoteByStreamId)
    }

    // ---- The monitor ---------------------------------------------------------

    private class Harness(scope: TestScope, micOn: Boolean = true) {
        val ui = CallUiState()
        var now = 0L
        var reading: CallAudioLevels? = null
        var readings = 0
        val monitor = SpeakingMonitor(
            scope = scope,
            ui = ui,
            sample = {
                readings += 1
                reading
            },
            participantIdOf = { "peer-$it" },
            nowMs = { now },
        )

        init {
            ui.setParticipants(listOf(person("local", isLocal = true, micOn = micOn), person("peer-a"), person("peer-b")))
            ui.setSpeakingWatched(true)
        }

        /** Feed the same reading every 100 ms over `[from, to]`. */
        suspend fun feed(levels: CallAudioLevels?, from: Long, to: Long) {
            reading = levels
            now = from
            while (now <= to) {
                monitor.tick()
                now += 100
            }
            now = to
        }

        fun speaking(): Set<String> = ui.participants.value.filter { it.speaking }.map { it.id }.toSet()
    }

    private val loud = SPEAKING_LEVEL * 5

    @Test
    fun `a voice puts the ring on that person's tile and nobody else's`() = runTest {
        val h = Harness(this)
        h.feed(CallAudioLevels(local = 0.0, remoteByStreamId = mapOf("a" to loud, "b" to 0.0)), 0, RING_ON_MS)
        assertEquals(setOf("peer-a"), h.speaking())
    }

    @Test
    fun `someone who keeps speaking takes the floor the stage follows`() = runTest {
        val h = Harness(this)
        h.feed(CallAudioLevels(local = 0.0, remoteByStreamId = mapOf("b" to loud)), 0, FLOOR_ONSET_MS)
        assertEquals("peer-b", h.ui.speech.value.activeSpeakerId)
        assertEquals(FLOOR_ONSET_MS, h.ui.speech.value.lastSpokeAtMs["peer-b"])
    }

    @Test
    fun `this device's own voice rings its own tile, unless it is muted`() = runTest {
        val open = Harness(this)
        open.feed(CallAudioLevels(local = loud, remoteByStreamId = emptyMap()), 0, RING_ON_MS)
        assertEquals(setOf("local"), open.speaking())

        val muted = Harness(this, micOn = false)
        muted.feed(CallAudioLevels(local = loud, remoteByStreamId = emptyMap()), 0, RING_ON_MS)
        assertTrue(muted.speaking().isEmpty())
    }

    @Test
    fun `a reading that cannot be taken draws nothing`() = runTest {
        val h = Harness(this)
        h.feed(null, 0, FLOOR_ONSET_MS)
        assertTrue(h.speaking().isEmpty())
        assertEquals(StageSpeech.None, h.ui.speech.value)
    }

    @Test
    fun `a stream that belongs to nobody in the call is ignored`() = runTest {
        val h = Harness(this)
        h.feed(CallAudioLevels(local = null, remoteByStreamId = mapOf("stranger" to loud)), 0, FLOOR_ONSET_MS)
        assertTrue(h.speaking().isEmpty())
        assertNull(h.ui.speech.value.activeSpeakerId)
    }

    @Test
    fun `nothing is read while no screen is showing the tiles`() = runTest {
        val h = Harness(this)
        h.ui.setSpeakingWatched(false)
        h.feed(CallAudioLevels(local = loud, remoteByStreamId = mapOf("a" to loud)), 0, FLOOR_ONSET_MS)
        assertEquals(0, h.readings)
        assertTrue(h.speaking().isEmpty())
    }

    @Test
    fun `someone who left is forgotten on the next reading`() = runTest {
        val h = Harness(this)
        h.feed(CallAudioLevels(local = 0.0, remoteByStreamId = mapOf("a" to loud)), 0, FLOOR_ONSET_MS)
        assertEquals("peer-a", h.ui.speech.value.activeSpeakerId)
        h.ui.participantLeft("peer-a")
        h.feed(CallAudioLevels(local = 0.0, remoteByStreamId = emptyMap()), FLOOR_ONSET_MS + 100, FLOOR_ONSET_MS + 100)
        assertNull(h.ui.speech.value.activeSpeakerId)
        assertFalse("peer-a" in h.ui.speech.value.lastSpokeAtMs)
    }

    @Test
    fun `stopping takes every ring down`() = runTest {
        val h = Harness(this)
        h.feed(CallAudioLevels(local = 0.0, remoteByStreamId = mapOf("a" to loud)), 0, FLOOR_ONSET_MS)
        assertEquals(setOf("peer-a"), h.speaking())
        h.monitor.stop()
        assertTrue(h.speaking().isEmpty())
        assertEquals(StageSpeech.None, h.ui.speech.value)
    }
}
