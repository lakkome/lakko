// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * [runScreenPollLoop] — the chat screen's receive loop around a membership
 * that ends and comes back (remove, then add again, with the conversation
 * open). Seen on an emulator 2026-11-23: the loop ended for good, the screen
 * kept its claim, the app-level poll left the group out, and the rows of the
 * new membership (the group's name among them) were read minutes later.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ScreenPollLoopTest {

    private class Claims {
        var claimed = 0
        var released = 0
        val held get() = claimed - released
        fun claim() = ScreenPollClaim(claim = { claimed += 1 }, release = { released += 1 })
    }

    @Test
    fun `a membership that comes back is polled again, and the claim follows it`() = runTest {
        val ended = MutableStateFlow(false)
        val claims = Claims()
        val claim = claims.claim()
        claim.ensure()
        var polls = 0
        val job = launch {
            runScreenPollLoop(
                ended = ended,
                claim = claim,
                poll = {
                    polls += 1
                    // The removal landed on a poll this loop drove.
                    if (polls == 2) ended.value = true
                    yield()
                    GroupPollOutcome.APPLIED
                },
                backoff = {},
            )
        }
        advanceUntilIdle()
        assertEquals("no poll while the membership has ended", 2, polls)
        assertEquals("the claim is given up while nothing polls", 0, claims.held)

        // Added back while the conversation is open.
        ended.value = false
        repeat(5) { yield() }
        assertTrue("polling resumes", polls > 2)
        assertEquals("the claim is taken back", 1, claims.held)
        job.cancel()
        job.join()
    }

    @Test
    fun `a screen that opens on a state still reading as ended does not keep the group from being polled`() = runTest {
        val ended = MutableStateFlow(true)
        val claims = Claims()
        val claim = claims.claim()
        claim.ensure()
        var polls = 0
        val job = launch {
            runScreenPollLoop(ended, claim, poll = { polls += 1; yield(); GroupPollOutcome.APPLIED }, backoff = {})
        }
        advanceUntilIdle()
        assertEquals(0, polls)
        assertFalse("the app-level poll may take the group", claim.held)
        ended.value = false
        repeat(5) { yield() }
        assertTrue(polls > 0)
        assertTrue(claim.held)
        job.cancel()
        job.join()
    }

    @Test
    fun `unavailable and a failure back off and the loop goes on`() = runTest {
        val ended = MutableStateFlow(false)
        val claim = Claims().claim()
        var polls = 0
        var backoffs = 0
        val job = launch {
            runScreenPollLoop(
                ended = ended,
                claim = claim,
                poll = {
                    polls += 1
                    when (polls) {
                        1 -> GroupPollOutcome.UNAVAILABLE
                        2 -> throw IllegalStateException("network")
                        else -> {
                            ended.value = true
                            GroupPollOutcome.APPLIED
                        }
                    }
                },
                backoff = { backoffs += 1 },
            )
        }
        advanceUntilIdle()
        assertEquals(3, polls)
        assertEquals(2, backoffs)
        job.cancel()
        job.join()
    }

    @Test
    fun `giving up twice releases once`() {
        val claims = Claims()
        val claim = claims.claim()
        claim.ensure()
        claim.ensure()
        claim.giveUp()
        claim.giveUp()
        assertEquals(1, claims.claimed)
        assertEquals(1, claims.released)
    }
}
