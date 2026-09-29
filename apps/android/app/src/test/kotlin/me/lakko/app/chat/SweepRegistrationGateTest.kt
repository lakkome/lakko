// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The hold after a refused sweep registration, and the owner claim a sweep
 * waits for before registering (ADR-0045 x ADR-0057, live walk 2026-09-29).
 */
class SweepRegistrationGateTest {

    private val initial = SweepRegistrationBackoff.INITIAL_DELAY_MS
    private val max = SweepRegistrationBackoff.MAX_DELAY_MS

    @Test
    fun `the first refusal holds for thirty seconds`() {
        val state = SweepRegistrationBackoff.afterRefusal(null, nowMs = 1_000L, epoch = 5uL)
        assertEquals(30_000L, initial)
        assertEquals(initial, state.delayMs)
        assertEquals(1_000L + initial, state.notBeforeMs)
    }

    @Test
    fun `each refusal at the same epoch doubles the hold up to ten minutes`() {
        var state: SweepRegistrationBackoff.State? = null
        val delays = mutableListOf<Long>()
        repeat(8) {
            state = SweepRegistrationBackoff.afterRefusal(state, nowMs = 0L, epoch = 5uL)
            delays.add(state!!.delayMs)
        }
        assertEquals(600_000L, max)
        assertEquals(listOf(30_000L, 60_000L, 120_000L, 240_000L, 480_000L, max, max, max), delays)
    }

    @Test
    fun `a refusal at another epoch starts over`() {
        val first = SweepRegistrationBackoff.afterRefusal(null, 0L, 5uL)
        val second = SweepRegistrationBackoff.afterRefusal(first, 0L, 5uL)
        val moved = SweepRegistrationBackoff.afterRefusal(second, 0L, 6uL)
        assertEquals(initial, moved.delayMs)
    }

    @Test
    fun `the hold ends at its deadline or when the epoch moves`() {
        val state = SweepRegistrationBackoff.afterRefusal(null, nowMs = 0L, epoch = 5uL)
        assertTrue(SweepRegistrationBackoff.mayStage(null, 0L, 5uL))
        assertFalse(SweepRegistrationBackoff.mayStage(state, initial - 1, 5uL))
        assertTrue(SweepRegistrationBackoff.mayStage(state, initial, 5uL))
        // A membership or role change advances the epoch: no wait.
        assertTrue(SweepRegistrationBackoff.mayStage(state, 1L, 6uL))
    }

    @Test
    fun `only explicit refusals start a hold`() {
        assertTrue(SweepRegistrationBackoff.isBackoffStatus(400))
        assertTrue(SweepRegistrationBackoff.isBackoffStatus(401))
        assertTrue(SweepRegistrationBackoff.isBackoffStatus(403))
        assertFalse(SweepRegistrationBackoff.isBackoffStatus(409))
        assertFalse(SweepRegistrationBackoff.isBackoffStatus(500))
    }

    @Test
    fun `the tracker holds per group and resets`() {
        var now = 0L
        val tracker = SweepRegistrationBackoffTracker(clock = { now })
        tracker.onRefused("aa", 3uL)
        assertFalse(tracker.mayStage("aa", 3uL))
        assertTrue("another group is not held", tracker.mayStage("bb", 3uL))
        now = initial
        assertTrue(tracker.mayStage("aa", 3uL))
        assertEquals(2 * initial, tracker.onRefused("aa", 3uL).delayMs)
        tracker.reset("aa")
        assertTrue(tracker.mayStage("aa", 3uL))
        assertEquals(initial, tracker.onRefused("aa", 3uL).delayMs)
    }

    /** An owner opening an unclaimed group sends its claim before any sweep
     *  registers an epoch: the sweep waits for the claim still in flight. */
    @Test
    fun `a sweep registers only after the claim launched on open`() = runTest {
        val gate = OwnerClaimGate()
        val order = mutableListOf<String>()
        val release = CompletableDeferred<Unit>()
        gate.start("aa", this) {
            order.add("claim sent")
            release.await()
            order.add("claim answered")
            true
        }
        val sweep = launch {
            gate.awaitBeforeRegistering("aa")
            order.add("register")
        }
        runCurrent() // no virtual time passes: the bound is not what releases it
        assertEquals(listOf("claim sent"), order)
        release.complete(Unit)
        sweep.join()
        assertEquals(listOf("claim sent", "claim answered", "register"), order)
    }

    @Test
    fun `with no claim in flight the sweep does not wait`() = runTest {
        val gate = OwnerClaimGate()
        gate.awaitBeforeRegistering("aa")
        val done = gate.start("aa", this) { true }
        done.join()
        gate.awaitBeforeRegistering("aa")
    }

    @Test
    fun `a claim that never answers holds the sweep only for the bound`() = runTest {
        val gate = OwnerClaimGate()
        val never = CompletableDeferred<Unit>()
        val claim = gate.start("aa", this) {
            never.await()
            true
        }
        gate.awaitBeforeRegistering("aa", timeoutMs = 1_000L)
        assertEquals(1_000L, testScheduler.currentTime)
        claim.cancel()
    }
}
