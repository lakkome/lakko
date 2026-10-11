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
}
