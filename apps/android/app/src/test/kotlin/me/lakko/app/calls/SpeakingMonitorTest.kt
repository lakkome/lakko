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
import uniffi.lakko_ffi_uniffi.SpeakingSnapshot

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
    //
    // The rule itself (the threshold, the ring's and the floor's timing, the
    // own person never holding the floor) is written and tested once, in
    // `crates/lakko-ui-rules/src/speaking.rs`. A host test cannot load the
    // native library, so these put a recording [SpeakingRule] in its place and
    // check what the monitor ASKS and what it does with the answer. They
    // cannot show that the rule behaves; `SpeakingRuleInstrumentedTest` asks
    // the real one, on a device.

    /** Records every question and answers what the test scripted. */
    private class FakeRule : SpeakingRule {
        val calls = ArrayList<String>()
        var answer = SpeakingSnapshot(speakingIds = emptyList(), activeSpeakerId = null, lastSpokeAtMs = emptyMap())
        var thresholds: Map<String, Double> = emptyMap()
        var closed = 0

        override fun setLocal(person: String?) {
            calls += "local($person)"
        }

        override fun sample(person: String, level: Double?, nowMs: ULong) {
            calls += "sample($person, $level, $nowMs)"
        }

        override fun silence(person: String) {
            calls += "silence($person)"
        }

        override fun keepOnly(people: List<String>) {
            calls += "keep(${people.joinToString(",")})"
        }

        override fun decideFloor(order: List<String>, nowMs: ULong): SpeakingSnapshot {
            calls += "floor(${order.joinToString(",")}, $nowMs)"
            return answer
        }

        override fun threshold(person: String): Double = thresholds[person] ?: -1.0

        override fun close() {
            closed += 1
        }

        fun asked(prefix: String): List<String> = calls.filter { it.startsWith(prefix) }
    }

    private class Harness(scope: TestScope, micOn: Boolean = true, rules: SpeakingRules? = null) {
        val ui = CallUiState()
        var now = 0L
        var reading: CallAudioLevels? = null
        var readings = 0
        var beforeReadingReturns: () -> Unit = {}
        val made = ArrayList<FakeRule>()
        val reports = ArrayList<Pair<List<VoiceReport>, String?>>()
        val monitor = SpeakingMonitor(
            scope = scope,
            ui = ui,
            sample = {
                readings += 1
                beforeReadingReturns()
                reading
            },
            participantIdOf = { "peer-$it" },
            nowMs = { now },
            rules = rules ?: SpeakingRules { FakeRule().also { made += it } },
            onReport = { voices, floorId -> reports += voices to floorId },
        )

        /** The rule of the call; made at the first reading. */
        val rule: FakeRule get() = made.last()

        init {
            ui.setParticipants(listOf(person("local", isLocal = true, micOn = micOn), person("peer-a"), person("peer-b")))
            ui.setSpeakingWatched(true)
        }

        /** One reading at [at]. */
        suspend fun read(levels: CallAudioLevels?, at: Long) {
            reading = levels
            now = at
            monitor.tick()
        }

        fun speaking(): Set<String> = ui.participants.value.filter { it.speaking }.map { it.id }.toSet()
    }

    private val quiet = CallAudioLevels(local = 0.0, remoteByStreamId = mapOf("a" to 0.0, "b" to 0.0))

    @Test
    fun `the rule is told who this device's own person is, once`() = runTest {
        val h = Harness(this)
        h.read(quiet, 0)
        h.read(quiet, 100)
        assertEquals(listOf("local(local)"), h.rule.asked("local"))
        // Before anything is sampled or decided.
        assertEquals("local(local)", h.rule.calls.first())
        assertEquals(1, h.made.size)
    }

    @Test
    fun `each level read is handed over as it was read, with the time of the reading`() = runTest {
        val h = Harness(this)
        // A level above full scale is not corrected here: that is the rule's.
        h.read(CallAudioLevels(local = 0.25, remoteByStreamId = mapOf("a" to 1.5, "b" to 0.0)), 700)
        assertEquals(
            listOf("sample(local, 0.25, 700)", "sample(peer-a, 1.5, 700)", "sample(peer-b, 0.0, 700)"),
            h.rule.asked("sample"),
        )
    }

    @Test
    fun `a level that could not be read is handed over as no level, not as silence`() = runTest {
        val h = Harness(this)
        h.read(CallAudioLevels(local = null, remoteByStreamId = mapOf("a" to 0.3)), 0)
        assertEquals(
            listOf("sample(local, null, 0)", "sample(peer-a, 0.3, 0)", "sample(peer-b, null, 0)"),
            h.rule.asked("sample"),
        )
    }

    @Test
    fun `a muted person is silenced and never sampled, this device's own included`() = runTest {
        val h = Harness(this, micOn = false)
        h.ui.setParticipants(
            listOf(person("local", isLocal = true, micOn = false), person("peer-a"), person("peer-b", micOn = false)),
        )
        h.read(CallAudioLevels(local = 0.9, remoteByStreamId = mapOf("a" to 0.3, "b" to 0.9)), 0)
        assertEquals(listOf("silence(local)", "silence(peer-b)"), h.rule.asked("silence"))
        assertEquals(listOf("sample(peer-a, 0.3, 0)"), h.rule.asked("sample"))
        // Unmuted: read again.
        h.ui.setMic(true)
        h.read(CallAudioLevels(local = 0.9, remoteByStreamId = mapOf("a" to 0.3, "b" to 0.9)), 100)
        assertEquals(listOf("silence(local)", "silence(peer-b)", "silence(peer-b)"), h.rule.asked("silence"))
        assertTrue("sample(local, 0.9, 100)" in h.rule.calls)
    }

    @Test
    fun `the floor is asked for with the whole roster in order, the own person included`() = runTest {
        val h = Harness(this)
        h.read(quiet, 450)
        assertEquals(listOf("floor(local,peer-a,peer-b, 450)"), h.rule.asked("floor"))
        // After every sample of the reading.
        assertEquals("floor(local,peer-a,peer-b, 450)", h.rule.calls.last())
    }

    @Test
    fun `the rule is told the roster when it changes, not at every reading`() = runTest {
        val h = Harness(this)
        h.read(quiet, 0)
        h.read(quiet, 100)
        assertEquals(listOf("keep(local,peer-a,peer-b)"), h.rule.asked("keep"))
        h.ui.participantLeft("peer-a")
        h.read(quiet, 200)
        assertEquals(listOf("keep(local,peer-a,peer-b)", "keep(local,peer-b)"), h.rule.asked("keep"))
        // The one who left is asked about no more.
        assertEquals("floor(local,peer-b, 200)", h.rule.calls.last())
        assertFalse(h.rule.calls.any { it.startsWith("sample(peer-a") && it.endsWith(" 200)") })
    }

    @Test
    fun `a stream that belongs to nobody in the call is not handed over`() = runTest {
        val h = Harness(this)
        h.read(CallAudioLevels(local = null, remoteByStreamId = mapOf("stranger" to 0.9)), 0)
        assertFalse(h.rule.calls.any { "stranger" in it })
    }

    @Test
    fun `a clock before its origin reads as the origin`() = runTest {
        val h = Harness(this)
        h.read(quiet, -5_000)
        assertTrue(h.rule.asked("sample").all { it.endsWith(", 0)") })
        assertEquals(listOf("floor(local,peer-a,peer-b, 0)"), h.rule.asked("floor"))
    }

    @Test
    fun `the rule's answer is what the tiles and the stage show`() = runTest {
        val h = Harness(this)
        h.read(quiet, 0)
        assertTrue(h.speaking().isEmpty())
        h.rule.answer = SpeakingSnapshot(
            speakingIds = listOf("local", "peer-b"),
            activeSpeakerId = "peer-b",
            lastSpokeAtMs = mapOf("peer-a" to 40uL, "peer-b" to 100uL),
        )
        h.read(quiet, 100)
        assertEquals(setOf("local", "peer-b"), h.speaking())
        assertEquals(
            StageSpeech(activeSpeakerId = "peer-b", lastSpokeAtMs = mapOf("peer-a" to 40L, "peer-b" to 100L)),
            h.ui.speech.value,
        )
        // And it is taken back when the rule takes it back.
        h.rule.answer = SpeakingSnapshot(emptyList(), null, mapOf("peer-b" to 100uL))
        h.read(quiet, 200)
        assertTrue(h.speaking().isEmpty())
        assertNull(h.ui.speech.value.activeSpeakerId)
        assertEquals(mapOf("peer-b" to 100L), h.ui.speech.value.lastSpokeAtMs)
    }

    @Test
    fun `every thirteenth reading reports each person's source, peak, ring and the rule's threshold, and the floor`() =
        runTest {
            assertEquals(13, SPEAKING_REPORT_EVERY_TICKS)
            val h = Harness(this, micOn = false)
            val levels = CallAudioLevels(
                local = 0.5,
                remoteByStreamId = mapOf("a" to 0.1, "b" to 0.01),
                remotePathByStreamId = mapOf("a" to LevelPath.STATS_ENERGY),
            )
            h.read(levels, 0)
            h.rule.thresholds = mapOf("local" to 0.02, "peer-a" to 0.031, "peer-b" to 0.007)
            // The ring was on for one reading of the interval: that counts.
            h.rule.answer = SpeakingSnapshot(listOf("peer-a"), null, emptyMap())
            h.read(levels.copy(remoteByStreamId = mapOf("a" to 0.4, "b" to 0.01)), 100)
            h.rule.answer = SpeakingSnapshot(emptyList(), "peer-a", emptyMap())
            for (i in 2..11) h.read(levels, i * 100L)
            assertTrue(h.reports.isEmpty())
            h.read(levels, 1_200)
            val (voices, floorId) = h.reports.single()
            assertEquals("peer-a", floorId)
            assertEquals(
                listOf(
                    // Muted: not read at all.
                    VoiceReport("local", isLocal = true, path = LevelPath.NONE, peak = null, speaking = false, threshold = 0.02),
                    VoiceReport("peer-a", false, LevelPath.STATS_ENERGY, peak = 0.4, speaking = true, threshold = 0.031),
                    VoiceReport("peer-b", false, LevelPath.STATS_LEVEL, peak = 0.01, speaking = false, threshold = 0.007),
                ),
                voices,
            )
            // The next report starts from nothing.
            for (i in 13..25) h.read(levels, i * 100L)
            assertEquals(2, h.reports.size)
            assertEquals(listOf(null, 0.1, 0.01), h.reports.last().first.map { it.peak })
            assertFalse(h.reports.last().first[1].speaking)
        }

    @Test
    fun `a reading that cannot be taken asks the rule nothing and draws nothing`() = runTest {
        val h = Harness(this)
        h.read(null, 0)
        assertTrue(h.made.isEmpty())
        assertTrue(h.speaking().isEmpty())
        assertEquals(StageSpeech.None, h.ui.speech.value)
    }

    @Test
    fun `nothing is read while no screen is showing the tiles`() = runTest {
        val h = Harness(this)
        h.ui.setSpeakingWatched(false)
        h.read(quiet, 0)
        assertEquals(0, h.readings)
        assertTrue(h.made.isEmpty())
    }

    @Test
    fun `stopping takes every ring down and closes the rule`() = runTest {
        val h = Harness(this)
        h.read(quiet, 0)
        h.rule.answer = SpeakingSnapshot(listOf("peer-a"), "peer-a", mapOf("peer-a" to 100uL))
        h.read(quiet, 100)
        assertEquals(setOf("peer-a"), h.speaking())
        h.monitor.stop()
        assertTrue(h.speaking().isEmpty())
        assertEquals(StageSpeech.None, h.ui.speech.value)
        assertEquals(1, h.rule.closed)
        // Stopping again closes nothing twice.
        h.monitor.stop()
        assertEquals(1, h.rule.closed)
    }

    @Test
    fun `a call read again after a stop has a new rule, told everything again`() = runTest {
        val h = Harness(this)
        h.read(quiet, 0)
        h.monitor.stop()
        h.read(quiet, 100)
        assertEquals(2, h.made.size)
        assertEquals(0, h.rule.closed)
        assertEquals(
            listOf("local(local)", "keep(local,peer-a,peer-b)"),
            h.rule.calls.take(2),
        )
    }

    @Test
    fun `a reading in flight when the monitor stops is dropped`() = runTest {
        val h = Harness(this)
        h.read(quiet, 0)
        val first = h.rule
        first.answer = SpeakingSnapshot(listOf("peer-a"), "peer-a", emptyMap())
        val before = first.calls.size
        h.beforeReadingReturns = { h.monitor.stop() }
        h.read(quiet, 100)
        assertEquals(1, h.made.size)
        assertEquals(1, first.closed)
        // Nothing is asked of a closed rule, and no ring comes back.
        assertEquals(before, first.calls.size)
        assertTrue(h.speaking().isEmpty())
    }

    @Test
    fun `while the rule cannot be had nothing is drawn, and it is asked for again`() = runTest {
        var asked = 0
        val h = Harness(
            this,
            rules = SpeakingRules {
                asked += 1
                throw UnsatisfiedLinkError("no library")
            },
        )
        h.read(CallAudioLevels(local = 0.9, remoteByStreamId = mapOf("a" to 0.9)), 0)
        h.read(CallAudioLevels(local = 0.9, remoteByStreamId = mapOf("a" to 0.9)), 100)
        assertEquals(2, asked)
        assertTrue(h.speaking().isEmpty())
        assertEquals(StageSpeech.None, h.ui.speech.value)
        h.monitor.stop()
    }
}
