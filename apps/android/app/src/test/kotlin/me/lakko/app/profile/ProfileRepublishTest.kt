// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0048 §2 — the publish triggers: join and profile change publish at once;
 * the group gaining a member republishes throttled (one per 30 s per group) and
 * jittered (0..10 s), never dropped.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ProfileRepublishTest {

    @Test
    fun `the growth delay is jitter alone with no recent publish`() {
        assertEquals(4_000L, growthRepublishDelayMs(100_000, lastPublishedAtMs = null, alreadyScheduled = false, jitterMs = 4_000))
        // The last publish is more than 30 s old: no throttle left.
        assertEquals(4_000L, growthRepublishDelayMs(100_000, lastPublishedAtMs = 60_000, alreadyScheduled = false, jitterMs = 4_000))
    }

    @Test
    fun `the growth delay waits out the 30 second throttle plus jitter`() {
        // Published 10 s ago: 20 s of throttle left, plus 3 s jitter.
        assertEquals(
            23_000L,
            growthRepublishDelayMs(110_000, lastPublishedAtMs = 100_000, alreadyScheduled = false, jitterMs = 3_000),
        )
    }

    @Test
    fun `growth while a republish is pending schedules nothing more`() {
        assertNull(growthRepublishDelayMs(nowMs = 0, lastPublishedAtMs = null, alreadyScheduled = true, jitterMs = 0))
    }

    @Test
    fun `jitter is clamped to its bound`() {
        assertEquals(PROFILE_REPUBLISH_MAX_JITTER_MS, growthRepublishDelayMs(0, null, false, 99_999))
        assertEquals(0L, growthRepublishDelayMs(0, null, false, -5))
    }

    @Test
    fun `growth is a new key, not a larger count`() {
        assertTrue(membershipGrew(listOf("A", "B"), listOf("A", "B", "C")))
        // Remove B and add C in one batch: same count, still a newcomer.
        assertTrue(membershipGrew(listOf("A", "B"), listOf("A", "C")))
        assertFalse(membershipGrew(listOf("A", "B"), listOf("A")))
        assertFalse(membershipGrew(listOf("A", "B"), listOf("B", "A")))
        // A torn read or an empty baseline decides nothing.
        assertFalse(membershipGrew(null, listOf("A")))
        assertFalse(membershipGrew(listOf("A"), null))
        assertFalse(membershipGrew(emptyList(), listOf("A")))
    }

    private class Harness(jitter: Long = 2_000L) {
        val scope = TestScope()
        val published = mutableListOf<Pair<String, Long>>()
        var succeed = true
        val scheduler = ProfileRepublishScheduler(
            scope = scope,
            nowMs = { scope.testScheduler.currentTime },
            jitterMs = { jitter },
            publish = { group ->
                published += group to scope.testScheduler.currentTime
                succeed
            },
        )
    }

    @Test
    fun `joining publishes at once`() {
        val h = Harness()
        h.scheduler.publishNow("G1")
        h.scope.runCurrent()
        assertEquals(listOf("G1" to 0L), h.published)
    }

    @Test
    fun `a profile change publishes into every group at once`() {
        val h = Harness()
        h.scheduler.publishNowToAll(listOf("G1", "G2", "G3"))
        h.scope.runCurrent()
        assertEquals(listOf("G1", "G2", "G3"), h.published.map { it.first })
    }

    @Test
    fun `growth right after a publish is throttled and jittered, then published`() {
        val h = Harness(jitter = 2_000L)
        h.scheduler.publishNow("G1")
        h.scope.runCurrent()
        h.scope.advanceTimeBy(5_000)
        // 25 s of throttle left + 2 s jitter.
        assertEquals(27_000L, h.scheduler.onGrowth("G1"))
        h.scope.advanceTimeBy(26_999)
        h.scope.runCurrent()
        assertEquals(1, h.published.size)
        h.scope.advanceTimeBy(1)
        h.scope.runCurrent()
        assertEquals(listOf("G1" to 0L, "G1" to 32_000L), h.published)
    }

    @Test
    fun `a burst of growth yields one republish per group`() {
        val h = Harness(jitter = 1_000L)
        assertEquals(1_000L, h.scheduler.onGrowth("G1"))
        assertNull(h.scheduler.onGrowth("G1"))
        assertNull(h.scheduler.onGrowth("G1"))
        // Another group is independent.
        assertEquals(1_000L, h.scheduler.onGrowth("G2"))
        h.scope.advanceTimeBy(60_000)
        h.scope.runCurrent()
        assertEquals(listOf("G1", "G2"), h.published.map { it.first }.sorted())
    }

    @Test
    fun `a failed publish does not start the throttle`() {
        val h = Harness(jitter = 0L)
        h.succeed = false
        h.scheduler.publishNow("G1")
        h.scope.runCurrent()
        // No successful publish recorded: growth is not held back 30 s.
        assertEquals(0L, h.scheduler.onGrowth("G1"))
    }
}
