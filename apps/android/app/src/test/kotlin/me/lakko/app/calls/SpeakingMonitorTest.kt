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
        val levels = AudioLevelReader().read(
            listOf(
                RtcStat("inbound-rtp", mapOf("kind" to "audio", "trackIdentifier" to "track-a", "audioLevel" to 0.4)),
                RtcStat("inbound-rtp", mapOf("kind" to "audio", "trackIdentifier" to "track-b", "audioLevel" to 0.0)),
            ),
            tracks,
        )
        assertEquals(mapOf("peer-a-id" to 0.4, "peer-b-id" to 0.0), levels.remoteByStreamId)
        assertEquals(LevelPath.STATS_LEVEL, levels.remotePathByStreamId["peer-a-id"])
        assertNull(levels.local)
        assertEquals(LevelPath.NONE, levels.localPath)
    }

    @Test
    fun `this device's own level is the audio source's`() {
        val levels = AudioLevelReader().read(
            listOf(
                RtcStat("media-source", mapOf("kind" to "audio", "audioLevel" to 0.25f)),
                RtcStat("media-source", mapOf("kind" to "video")),
            ),
            tracks,
        )
        assertEquals(0.25, levels.local!!, 1e-6)
        assertEquals(LevelPath.STATS_LEVEL, levels.localPath)
        assertTrue(levels.remoteByStreamId.isEmpty())
    }

    @Test
    fun `what cannot be read is left out, never guessed`() {
        val levels = AudioLevelReader().read(
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
        val levels = AudioLevelReader().read(
            listOf(
                RtcStat("inbound-rtp", mapOf("kind" to "audio", "trackIdentifier" to "track-a", "audioLevel" to 0.1)),
                RtcStat("inbound-rtp", mapOf("kind" to "audio", "trackIdentifier" to "track-a", "audioLevel" to 0.6)),
            ),
            mapOf("track-a" to "peer-a-id"),
        )
        assertEquals(mapOf("peer-a-id" to 0.6), levels.remoteByStreamId)
    }

    private fun energy(id: String, total: Double, duration: Double, level: Double? = null) = RtcStat(
        type = "inbound-rtp",
        id = id,
        members = mapOf(
            "kind" to "audio",
            "trackIdentifier" to "track-a",
            "totalAudioEnergy" to total,
            "totalSamplesDuration" to duration,
            "audioLevel" to level,
        ),
    )

    @Test
    fun `a level is the energy played since the last reading, once there is a last reading`() {
        val reader = AudioLevelReader()
        // The first reading has nothing to measure from: the entry's own level.
        val first = reader.read(listOf(energy("A", total = 1.0, duration = 10.0, level = 0.3)), tracks)
        assertEquals(0.3, first.remoteByStreamId.getValue("peer-a-id"), 1e-9)
        assertEquals(LevelPath.STATS_LEVEL, first.remotePathByStreamId["peer-a-id"])
        // 0.04 of energy over 1 s played: a root mean square of 0.2, whatever
        // the instantaneous level says.
        val second = reader.read(listOf(energy("A", total = 1.04, duration = 11.0, level = 0.9)), tracks)
        assertEquals(0.2, second.remoteByStreamId.getValue("peer-a-id"), 1e-9)
        assertEquals(LevelPath.STATS_ENERGY, second.remotePathByStreamId["peer-a-id"])
        // Nothing played since: silence, not the last level.
        val third = reader.read(listOf(energy("A", total = 1.04, duration = 11.0, level = 0.9)), tracks)
        assertEquals(0.0, third.remoteByStreamId.getValue("peer-a-id"), 0.0)
    }

    @Test
    fun `energy counters that went back start again, and a first reading with no level is left out`() {
        val reader = AudioLevelReader()
        assertTrue(reader.read(listOf(energy("A", 5.0, 50.0)), tracks).remoteByStreamId.isEmpty())
        // A new stream under the same id: nothing to measure from yet.
        assertTrue(reader.read(listOf(energy("A", 0.1, 1.0)), tracks).remoteByStreamId.isEmpty())
        val next = reader.read(listOf(energy("A", 0.35, 2.0)), tracks)
        assertEquals(0.5, next.remoteByStreamId.getValue("peer-a-id"), 1e-9)
        // A level is never above 1.
        val loud = reader.read(listOf(energy("A", 9.35, 3.0)), tracks)
        assertEquals(1.0, loud.remoteByStreamId.getValue("peer-a-id"), 0.0)
    }

    // ---- The monitor ---------------------------------------------------------

    private class Harness(scope: TestScope, micOn: Boolean = true) {
        val ui = CallUiState()
        var now = 0L
        var reading: CallAudioLevels? = null
        var readings = 0
        val reports = ArrayList<Pair<List<VoiceReport>, String?>>()
        val monitor = SpeakingMonitor(
            scope = scope,
            ui = ui,
            sample = {
                readings += 1
                reading
            },
            participantIdOf = { "peer-$it" },
            nowMs = { now },
            onReport = { voices, floorId -> reports += voices to floorId },
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
    fun `this device's own voice never takes the floor, however long it lasts`() = runTest {
        val h = Harness(this)
        h.feed(CallAudioLevels(local = loud, remoteByStreamId = emptyMap()), 0, 3 * FLOOR_ONSET_MS)
        assertEquals(setOf("local"), h.speaking())
        assertNull(h.ui.speech.value.activeSpeakerId)
        assertTrue(h.ui.speech.value.lastSpokeAtMs.isEmpty())
        // And it does not keep the floor from the person who then speaks.
        h.feed(
            CallAudioLevels(local = loud, remoteByStreamId = mapOf("b" to loud)),
            3 * FLOOR_ONSET_MS + 100,
            5 * FLOOR_ONSET_MS,
        )
        assertEquals("peer-b", h.ui.speech.value.activeSpeakerId)
    }

    @Test
    fun `the own level is not looked at while the microphone is off`() = runTest {
        val h = Harness(this)
        h.feed(CallAudioLevels(local = loud, remoteByStreamId = emptyMap()), 0, RING_ON_MS)
        assertEquals(setOf("local"), h.speaking())
        // Muted mid-sentence: the ring goes off at once, not after the pause.
        h.ui.setMic(false)
        h.feed(CallAudioLevels(local = loud, remoteByStreamId = emptyMap()), RING_ON_MS + 100, RING_ON_MS + 100)
        assertTrue(h.speaking().isEmpty())
    }

    @Test
    fun `someone whose stream stopped reporting falls silent`() = runTest {
        val h = Harness(this)
        h.feed(CallAudioLevels(local = 0.0, remoteByStreamId = mapOf("a" to loud)), 0, RING_ON_MS)
        assertEquals(setOf("peer-a"), h.speaking())
        // No entry for them any more: read as silence, so the ring goes off.
        h.feed(
            CallAudioLevels(local = 0.0, remoteByStreamId = emptyMap()),
            RING_ON_MS + 100,
            RING_ON_MS + 100 + RING_OFF_MS,
        )
        assertTrue(h.speaking().isEmpty())
    }

    @Test
    fun `every thirteenth reading reports each person's source, peak and ring, and the floor`() = runTest {
        assertEquals(13, SPEAKING_REPORT_EVERY_TICKS)
        val h = Harness(this)
        h.ui.setMic(false)
        val levels = CallAudioLevels(
            local = loud,
            remoteByStreamId = mapOf("a" to loud, "b" to 0.01),
            remotePathByStreamId = mapOf("a" to LevelPath.STATS_ENERGY),
        )
        h.feed(levels, 0, 1_100)
        assertTrue(h.reports.isEmpty())
        h.feed(levels, 1_200, 1_200)
        val (voices, floorId) = h.reports.single()
        assertNull(floorId)
        // The threshold in force for each person: the muted one was never
        // read and keeps the start value; the two others' floors climbed for
        // 1.2 s from the start value, neither as far as its own level.
        val climbed = SPEAKING_LEVEL * Math.pow(NOISE_RISE_PER_SECOND, 1.2)
        assertEquals(SPEAKING_LEVEL, voices[0].threshold, 0.0)
        assertEquals(climbed, voices[1].threshold, 1e-9)
        assertEquals(climbed, voices[2].threshold, 1e-9)
        assertEquals(
            listOf(
                // Muted: not read at all.
                VoiceReport("local", isLocal = true, path = LevelPath.NONE, peak = null, speaking = false),
                VoiceReport("peer-a", isLocal = false, path = LevelPath.STATS_ENERGY, peak = loud, speaking = true),
                VoiceReport("peer-b", isLocal = false, path = LevelPath.STATS_LEVEL, peak = 0.01, speaking = false),
            ),
            voices.map { it.copy(threshold = SPEAKING_LEVEL) },
        )
        // The next report starts from nothing and names the floor.
        h.feed(levels, 1_300, 2_500)
        assertEquals(2, h.reports.size)
        assertEquals("peer-a", h.reports.last().second)
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
