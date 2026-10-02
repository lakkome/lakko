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
