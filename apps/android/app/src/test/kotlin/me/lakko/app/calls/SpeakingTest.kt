// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Who is speaking (`docs/ux/group-call-grid.md` sections 5.2 and 7.3). The
 * test list is the web client's `speaking.test.ts`, so the two trackers are
 * held to the same behaviour.
 */
class SpeakingTest {
    private val loud = SPEAKING_LEVEL * 5
    private val order = listOf("a", "b")

    /** Feed [id] one level every 100 ms over `[from, to]`. */
    private fun feed(t: SpeakingTracker, id: String, level: Double, from: Long, to: Long) {
        var now = from
        while (now <= to) {
            t.sample(id, level, now)
            now += 100
        }
    }

    @Test
    fun `the four numbers are the web client's`() {
        assertEquals(0.02, SPEAKING_LEVEL, 0.0)
        assertEquals(200L, RING_ON_MS)
        assertEquals(800L, RING_OFF_MS)
        assertEquals(1_500L, FLOOR_ONSET_MS)
    }

    // ---- The ring ------------------------------------------------------------

    @Test
    fun `the ring comes on after a short sound and goes off after a pause`() {
        val t = SpeakingTracker()
        t.sample("a", loud, 0)
        assertFalse("a" in t.snapshot(order, 0).speakingIds)
        t.sample("a", loud, RING_ON_MS)
        assertTrue("a" in t.snapshot(order, RING_ON_MS).speakingIds)
        t.sample("a", 0.0, RING_ON_MS + 100)
        assertTrue("a" in t.snapshot(order, RING_ON_MS + 100).speakingIds)
        t.sample("a", 0.0, RING_ON_MS + 100 + RING_OFF_MS)
        assertFalse("a" in t.snapshot(order, RING_ON_MS + 100 + RING_OFF_MS).speakingIds)
    }

    @Test
    fun `the ring is on for everyone who is speaking at once`() {
        val t = SpeakingTracker()
        feed(t, "a", loud, 0, 500)
        feed(t, "b", loud, 0, 500)
        assertEquals(setOf("a", "b"), t.snapshot(order, 500).speakingIds)
    }

    @Test
    fun `a level that is not a number is silence`() {
        val t = SpeakingTracker()
        feed(t, "a", Double.NaN, 0, 500)
        feed(t, "b", Double.POSITIVE_INFINITY, 0, 500)
        assertTrue(t.snapshot(order, 500).speakingIds.isEmpty())
    }

    // ---- The floor -----------------------------------------------------------

    @Test
    fun `the floor is not taken by a cough`() {
        val t = SpeakingTracker()
        feed(t, "a", loud, 0, 600)
        assertNull(t.snapshot(order, 600).activeSpeakerId)
    }

    @Test
    fun `the floor goes to someone who keeps speaking, and is remembered after they stop`() {
        val t = SpeakingTracker()
        feed(t, "a", loud, 0, FLOOR_ONSET_MS)
        val during = t.snapshot(order, FLOOR_ONSET_MS)
        assertEquals("a", during.activeSpeakerId)
        feed(t, "a", 0.0, FLOOR_ONSET_MS + 100, FLOOR_ONSET_MS + 100 + RING_OFF_MS)
        val after = t.snapshot(order, FLOOR_ONSET_MS + 100 + RING_OFF_MS)
        assertNull(after.activeSpeakerId)
        assertEquals(FLOOR_ONSET_MS, after.lastSpokeAtMs["a"])
    }

    @Test
    fun `the floor stays with whoever holds it while a second person talks over them`() {
        val t = SpeakingTracker()
        feed(t, "b", loud, 0, FLOOR_ONSET_MS)
        assertEquals("b", t.snapshot(order, FLOOR_ONSET_MS).activeSpeakerId)
        // `a` is earlier in the roster and now qualifies too; `b` keeps the floor.
        feed(t, "a", loud, FLOOR_ONSET_MS, 2 * FLOOR_ONSET_MS + 200)
        feed(t, "b", loud, FLOOR_ONSET_MS, 2 * FLOOR_ONSET_MS + 200)
        assertEquals("b", t.snapshot(order, 2 * FLOOR_ONSET_MS + 200).activeSpeakerId)
    }

    @Test
    fun `this device's own voice never takes the floor, and does not keep others from it`() {
        val t = SpeakingTracker()
        // The own microphone is sampled (its tile has a ring) and has spoken
        // for longer than anyone; it is not a candidate.
        feed(t, "me", loud, 0, 2 * FLOOR_ONSET_MS)
        val alone = t.snapshot(listOf("a", "b"), 2 * FLOOR_ONSET_MS)
        assertNull(alone.activeSpeakerId)
        assertTrue("me" in alone.speakingIds)
        assertNull(alone.lastSpokeAtMs["me"])
        // Someone else starts while the own microphone goes on: they take it.
        feed(t, "me", loud, 2 * FLOOR_ONSET_MS, 4 * FLOOR_ONSET_MS)
        feed(t, "b", loud, 2 * FLOOR_ONSET_MS, 4 * FLOOR_ONSET_MS)
        assertEquals("b", t.snapshot(listOf("a", "b"), 4 * FLOOR_ONSET_MS).activeSpeakerId)
    }

    @Test
    fun `a floor held by someone who is no longer a candidate is given up`() {
        val t = SpeakingTracker()
        feed(t, "a", loud, 0, FLOOR_ONSET_MS)
        assertEquals("a", t.snapshot(order, FLOOR_ONSET_MS).activeSpeakerId)
        feed(t, "a", loud, FLOOR_ONSET_MS, FLOOR_ONSET_MS + 300)
        assertNull(t.snapshot(listOf("b"), FLOOR_ONSET_MS + 300).activeSpeakerId)
    }

    @Test
    fun `the time the ring stays on after the last sound does not count towards the onset`() {
        val t = SpeakingTracker()
        // A sound of 0.9 s, then silence: the ring stays on for another 0.8 s,
        // which together is past the onset. The speech itself was not.
        feed(t, "a", loud, 0, 900)
        feed(t, "a", 0.0, 1_000, 1_600)
        val snap = t.snapshot(order, 1_600)
        assertTrue("a" in snap.speakingIds)
        assertNull(snap.activeSpeakerId)
    }

    @Test
    fun `the holder keeps the floor through a pause shorter than the ring's`() {
        val t = SpeakingTracker()
        feed(t, "a", loud, 0, FLOOR_ONSET_MS)
        assertEquals("a", t.snapshot(order, FLOOR_ONSET_MS).activeSpeakerId)
        // A breath: the ring is still on, so the floor is still held.
        feed(t, "a", 0.0, FLOOR_ONSET_MS + 100, FLOOR_ONSET_MS + 500)
        assertEquals("a", t.snapshot(order, FLOOR_ONSET_MS + 500).activeSpeakerId)
    }

    @Test
    fun `a muted person's ring goes off at once and they give up the floor`() {
        val t = SpeakingTracker()
        feed(t, "a", loud, 0, FLOOR_ONSET_MS)
        assertEquals("a", t.snapshot(order, FLOOR_ONSET_MS).activeSpeakerId)
        t.silence("a")
        val snap = t.snapshot(order, FLOOR_ONSET_MS + 10)
        assertFalse("a" in snap.speakingIds)
        assertNull(snap.activeSpeakerId)
        // They held it, which is still remembered for the large tile.
        assertEquals(FLOOR_ONSET_MS, snap.lastSpokeAtMs["a"])
        // Silencing someone never sampled does nothing.
        t.silence("nobody")
    }

    @Test
    fun `someone who left the call is forgotten`() {
        val t = SpeakingTracker()
        feed(t, "a", loud, 0, FLOOR_ONSET_MS)
        t.snapshot(order, FLOOR_ONSET_MS)
        t.keepOnly(setOf("b"))
        val snap = t.snapshot(listOf("b"), FLOOR_ONSET_MS + 100)
        assertNull(snap.activeSpeakerId)
        assertNull(snap.lastSpokeAtMs["a"])
        assertTrue(snap.speakingIds.isEmpty())
    }

    /**
     * The first reading, counted from the first one with sound, at which
     * [read] is true when a person speaks without a pause and is read every
     * [intervalMs].
     */
    private fun firstReadingWhere(intervalMs: Long, read: (SpeakingSnapshot) -> Boolean): Long {
        val t = SpeakingTracker()
        var now = 0L
        while (now <= 10_000) {
            t.sample("a", loud, now)
            if (read(t.snapshot(order, now))) return now
            now += intervalMs
        }
        error("never")
    }

    @Test
    fun `the floor is taken at the same moment whether read every 100 ms or every 150`() {
        // This client reads every 150 ms and the web client every 100. The
        // thresholds are times, not numbers of readings, and the onset is a
        // whole number of both intervals.
        assertEquals(150L, SPEAKING_SAMPLE_INTERVAL_MS)
        assertEquals(0L, FLOOR_ONSET_MS % 100)
        assertEquals(0L, FLOOR_ONSET_MS % SPEAKING_SAMPLE_INTERVAL_MS)
        assertEquals(FLOOR_ONSET_MS, firstReadingWhere(100) { it.activeSpeakerId == "a" })
        assertEquals(FLOOR_ONSET_MS, firstReadingWhere(SPEAKING_SAMPLE_INTERVAL_MS) { it.activeSpeakerId == "a" })
        // The ring is where the grids differ: the first reading at or past
        // 200 ms is the one at 300 ms on this client's grid.
        assertEquals(200L, firstReadingWhere(100) { "a" in it.speakingIds })
        assertEquals(300L, firstReadingWhere(SPEAKING_SAMPLE_INTERVAL_MS) { "a" in it.speakingIds })
    }

    // ---- What counts as sound (the web client's `NoiseGate` cases) -----------

    /** Feed one level every [everyMs] for [ms], starting at [from]. */
    private fun hold(gate: NoiseGate, level: Double, from: Long, ms: Long, everyMs: Long = 100): Long {
        var now = from
        while (now < from + ms) {
            gate.update(level, now)
            now += everyMs
        }
        return now
    }

    @Test
    fun `the gate's numbers are the web client's`() {
        assertEquals(3.0, SPEECH_OVER_NOISE, 0.0)
        assertEquals(0.005, SPEAKING_LEVEL_MIN, 0.0)
        assertEquals(0.05, SPEAKING_LEVEL_MAX, 0.0)
        assertEquals(1.1, NOISE_RISE_PER_SECOND, 0.0)
        assertEquals(0.5, NOISE_FALL_PER_SAMPLE, 0.0)
        assertEquals(100L, NOISE_FALL_SAMPLE_MS)
        assertEquals(1e-4, NOISE_EVIDENCE_MIN, 0.0)
    }

    @Test
    fun `the threshold starts at the fixed level before anything is known`() {
        assertEquals(SPEAKING_LEVEL, NoiseGate().threshold, 1e-9)
        assertEquals(SPEAKING_LEVEL, SpeakingTracker().thresholdOf("nobody"), 0.0)
    }

    @Test
    fun `it follows a quiet room down, to a bound`() {
        for (every in listOf(100L, SPEAKING_SAMPLE_INTERVAL_MS)) {
            val gate = NoiseGate()
            hold(gate, 0.002, 0, 1_000, every)
            assertEquals(0.002 * SPEECH_OVER_NOISE, gate.threshold, 1e-9)
            hold(gate, 0.0002, 1_050, 2_000, every)
            assertEquals(SPEAKING_LEVEL_MIN, gate.threshold, 0.0)
        }
    }

    @Test
    fun `one torn reading does not empty the floor`() {
        val gate = NoiseGate()
        hold(gate, 0.006, 0, 1_000)
        val before = gate.threshold
        // At the web client's interval: half, the web client's rule.
        gate.update(0.0002, 1_000)
        assertEquals(before / 2, gate.threshold, 1e-9)
    }

    @Test
    fun `the first reading may fall as far as one reading of the web client`() {
        val gate = NoiseGate()
        gate.update(0.0002, 0)
        assertEquals(SPEAKING_LEVEL / 2, gate.threshold, 1e-9)
    }

    @Test
    fun `the floor falls by the same factor per unit of time at 150 ms as the web client's does at 100`() {
        fun fallen(everyMs: Long, forMs: Long): Double {
            val gate = NoiseGate()
            // Learn a floor of 0.016 (threshold 0.048, inside the bounds).
            hold(gate, 0.016, 0, 30_000, everyMs)
            assertEquals(0.048, gate.threshold, 1e-9)
            // The first reading of the falling stretch comes [everyMs] after
            // the last one of the learning stretch.
            val from = 30_000L
            var now = from
            while (now - from < forMs) {
                gate.update(NOISE_EVIDENCE_MIN, now)
                now += everyMs
            }
            return gate.threshold
        }
        // One reading each: 0.5 at 100 ms, 0.5^1.5 (about 0.354) at 150 ms.
        assertEquals(0.048 * 0.5, fallen(100, 100), 1e-9)
        assertEquals(0.048 * Math.pow(0.5, 1.5), fallen(150, 150), 1e-9)
        // After 300 ms (three readings there, two here) both have fallen by
        // 0.5^3, and the result is still above the lower bound.
        assertEquals(0.048 * 0.125, fallen(100, 300), 1e-9)
        assertEquals(fallen(100, 300), fallen(150, 300), 1e-9)
        assertTrue(fallen(150, 300) > SPEAKING_LEVEL_MIN)
    }

    @Test
    fun `it learns nothing from digital silence or from a reading that is not a number`() {
        val gate = NoiseGate()
        hold(gate, 0.01, 0, 20_000)
        val before = gate.threshold
        hold(gate, 0.0, 20_000, 5_000)
        hold(gate, NOISE_EVIDENCE_MIN / 2, 25_000, 5_000)
        gate.update(Double.NaN, 30_000)
        gate.update(Double.POSITIVE_INFINITY, 30_100)
        assertEquals(before, gate.threshold, 0.0)
    }

    @Test
    fun `it climbs slowly towards steady sound and never past the bound or the sound itself`() {
        for (every in listOf(100L, SPEAKING_SAMPLE_INTERVAL_MS)) {
            val gate = NoiseGate()
            // Three seconds of speech barely move it.
            hold(gate, 0.2, 0, 3_000, every)
            assertTrue(gate.threshold < SPEAKING_LEVEL * 1.4)
            // A minute of the same steady sound stops at the bound.
            hold(gate, 0.2, 3_000, 60_000, every)
            assertEquals(SPEAKING_LEVEL_MAX, gate.threshold, 0.0)
            // A steady hum settles the floor at the hum, not above it.
            val hum = NoiseGate()
            hold(hum, 0.01, 0, 60_000, every)
            assertEquals(0.01 * SPEECH_OVER_NOISE, hum.threshold, 1e-9)
        }
    }

    @Test
    fun `the rise over a stretch of time does not depend on how often it is read`() {
        val web = NoiseGate()
        val here = NoiseGate()
        hold(web, 0.2, 0, 3_001, 100)
        hold(here, 0.2, 0, 3_001, SPEAKING_SAMPLE_INTERVAL_MS)
        // Both were last read at 3 000 ms.
        assertEquals(web.threshold, here.threshold, 1e-9)
    }

    @Test
    fun `a long gap between readings is not a long rise`() {
        val gate = NoiseGate()
        gate.update(0.2, 0)
        gate.update(0.2, 3_600_000)
        assertTrue(gate.threshold < SPEAKING_LEVEL * 1.2)
    }

    // ---- One scale for a loud room and a quiet microphone --------------------

    /** Speech as a call delivers it: syllables around [level], never steady. */
    private fun talk(t: SpeakingTracker, id: String, level: Double, from: Long, ms: Long, everyMs: Long): Long {
        var now = from
        var i = 0
        while (now < from + ms) {
            t.sample(id, level * (if (i % 3 == 0) 0.6 else 1.0), now)
            now += everyMs
            i += 1
        }
        return now
    }

    @Test
    fun `the ring is drawn for speech at 0_03 over a room at 0_002, which the fixed level missed`() {
        for (every in listOf(100L, SPEAKING_SAMPLE_INTERVAL_MS)) {
            val t = SpeakingTracker()
            var now = 0L
            while (now < 3_000) {
                t.sample("a", 0.002, now)
                now += every
            }
            assertFalse("a" in t.snapshot(order, now).speakingIds)
            // 0.6 * 0.03 = 0.018 is under the old fixed 0.02 every third reading.
            now = talk(t, "a", 0.03, now, FLOOR_ONSET_MS + 500, every)
            val snap = t.snapshot(order, now)
            assertTrue("a" in snap.speakingIds)
            assertEquals("a", snap.activeSpeakerId)
        }
    }

    @Test
    fun `it is not drawn for a room that idles at 0_015, and is for speech over that room`() {
        for (every in listOf(100L, SPEAKING_SAMPLE_INTERVAL_MS)) {
            val t = SpeakingTracker()
            var now = 0L
            while (now < 10_000) {
                t.sample("a", 0.015, now)
                assertFalse("a" in t.snapshot(order, now).speakingIds)
                now += every
            }
            now = talk(t, "a", 0.15, now, 1_000, every)
            assertTrue("a" in t.snapshot(order, now).speakingIds)
        }
    }

    @Test
    fun `a steady noise that came on stops counting within seconds`() {
        for (every in listOf(100L, SPEAKING_SAMPLE_INTERVAL_MS)) {
            val t = SpeakingTracker()
            var now = 0L
            while (now < 1_000) {
                t.sample("a", 0.03, now)
                now += every
            }
            assertTrue("a" in t.snapshot(order, now).speakingIds)
            while (now < 7_000) {
                t.sample("a", 0.03, now)
                now += every
            }
            assertFalse("a" in t.snapshot(order, now).speakingIds)
        }
    }

    @Test
    fun `what was learned about the room is kept across a mute`() {
        val t = SpeakingTracker()
        var now = 0L
        while (now < 3_000) {
            t.sample("a", 0.002, now)
            now += 100
        }
        val learned = t.thresholdOf("a")
        assertEquals(0.006, learned, 1e-9)
        t.silence("a")
        assertEquals(learned, t.thresholdOf("a"), 0.0)
    }

    @Test
    fun `a level that could not be read is silence for the ring and teaches the floor nothing`() {
        val t = SpeakingTracker()
        feed(t, "a", loud, 0, 500)
        assertTrue("a" in t.snapshot(order, 500).speakingIds)
        val before = t.thresholdOf("a")
        var now = 600L
        while (now <= 600 + RING_OFF_MS) {
            t.sample("a", null, now)
            now += 100
        }
        assertFalse("a" in t.snapshot(order, 600 + RING_OFF_MS).speakingIds)
        assertEquals(before, t.thresholdOf("a"), 0.0)
    }
}
