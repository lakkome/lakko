// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.calls.CALL_RING_WINDOW_MS
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/calls.md` section 2.1 — how often the app-level receive loop reaches
 * every group, which is what decides whether an incoming call can still be
 * ringing when its offer is read.
 *
 * ## The defect these pin
 *
 * A call rang only while its chat was open. That chat's screen runs its own
 * uninterrupted long poll; every OTHER group shared three lanes and was swept in
 * waves at a fixed 30 s per wave, so a four-chat account took 60 s to come back
 * round to any given group and a seven-chat account 90 s — both past the 45 s
 * ring window, and past the stale-offer cut-off at which the host stops ringing
 * for an offer at all.
 */
class AppPollPolicyTest {

    private val lanes = POLL_ALL_CONCURRENCY
    private val maxWait = 30

    /** The sweep period the previous fixed-wait policy produced. */
    private fun legacySweepSecs(groupCount: Int): Int =
        appPollWaves(groupCount, lanes) * maxWait

    @Test
    fun `a set that fits one wave is untouched`() {
        // The common case, and the one that was never broken: every group long
        // polls at once, so the sweep IS one wait and there is nothing to divide.
        for (n in 1..lanes) {
            assertEquals("n=$n", maxWait, appPollWaitSecs(n, lanes, maxWait))
            assertEquals("n=$n", maxWait, appPollSweepSecs(n, lanes, maxWait))
        }
    }

    @Test
    fun `every group is reached inside the ring window`() {
        // The headline. Up to a genuinely large account, a full sweep finishes
        // with time left for the callee to answer.
        val windowSecs = (CALL_RING_WINDOW_MS / 1000L).toInt()
        for (n in 1..18) {
            val sweep = appPollSweepSecs(n, lanes, maxWait)
            assertTrue(
                "n=$n swept in ${sweep}s, ring window is ${windowSecs}s",
                sweep < windowSecs,
            )
        }
    }

    @Test
    fun `the sweep that missed the window before now fits inside it`() {
        // Four chats: two waves. This is the reported repro's shape.
        assertEquals(60, legacySweepSecs(4))
        assertTrue(appPollSweepSecs(4, lanes, maxWait) <= APP_POLL_SWEEP_BUDGET_SECS)
        // Seven chats: three waves, and the old sweep was past even the host's
        // stale-offer cut-off, so the call was never rung at all.
        assertEquals(90, legacySweepSecs(7))
        assertTrue(appPollSweepSecs(7, lanes, maxWait) <= APP_POLL_SWEEP_BUDGET_SECS)
    }

    @Test
    fun `waves are the group count divided by the lanes, rounded up`() {
        assertEquals(0, appPollWaves(0, lanes))
        assertEquals(1, appPollWaves(1, lanes))
        assertEquals(1, appPollWaves(3, lanes))
        assertEquals(2, appPollWaves(4, lanes))
        assertEquals(3, appPollWaves(7, lanes))
        assertEquals(10, appPollWaves(30, lanes))
    }

    @Test
    fun `an immediate refresh is never turned into a long poll`() {
        // Pull-to-refresh passes 0 and means it; dividing zero across waves
        // would not be zero.
        assertEquals(0, appPollWaitSecs(20, lanes, maxWaitSecs = 0))
        assertEquals(0, appPollSweepSecs(20, lanes, maxWaitSecs = 0))
    }

    @Test
    fun `the caller's budget is never exceeded`() {
        for (n in 0..40) {
            assertTrue("n=$n", appPollWaitSecs(n, lanes, maxWait) <= maxWait)
        }
        // A caller whose own budget is below the floor gets its budget, not the
        // floor: the floor exists to bound load, never to lengthen a wait the
        // caller asked to be short.
        assertEquals(2, appPollWaitSecs(20, lanes, maxWaitSecs = 2))
    }

    @Test
    fun `the request rate stays bounded however many chats there are`() {
        // What keeps shortening the wait honest: only `lanes` polls are ever in
        // flight and none is shorter than the floor, so the rate can never
        // exceed lanes/floor no matter how large the account.
        val worstCaseRate = lanes.toDouble() / APP_POLL_MIN_WAIT_SECS
        for (n in 1..200) {
            val wait = appPollWaitSecs(n, lanes, maxWait)
            assertTrue("n=$n wait=$wait", wait >= APP_POLL_MIN_WAIT_SECS.coerceAtMost(maxWait))
            assertTrue("n=$n", lanes.toDouble() / wait <= worstCaseRate + 1e-9)
        }
    }

    @Test
    fun `a very large account sweeps far faster than before, and still not fast enough`() {
        // Stated rather than hidden. Thirty chats went from a five-minute sweep
        // to about fifty seconds, which is a six-fold improvement and still
        // outside the ring window; closing that needs one multiplexed DS
        // subscription rather than a poll per group.
        assertEquals(300, legacySweepSecs(30))
        val sweep = appPollSweepSecs(30, lanes, maxWait)
        assertTrue("swept in ${sweep}s", sweep in 40..60)
        assertTrue(sweep > (CALL_RING_WINDOW_MS / 1000L).toInt())
    }
}
