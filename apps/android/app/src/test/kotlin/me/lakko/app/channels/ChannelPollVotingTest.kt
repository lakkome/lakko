// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0092 section 4.3 — the channel-poll vote write and its retries. */
class ChannelPollVotingTest {

    @Test
    fun `a stored vote takes one write`() = runBlocking {
        val ats = mutableListOf<Long>()
        assertTrue(castChannelVote({ 1_000_000L }) { at -> ats += at; ChannelVoteAnswer.Stored })
        assertEquals(listOf(1_000_000L), ats)
    }

    @Test
    fun `a stale vote is retried just after the stored one while inside the window`() = runBlocking {
        val ats = mutableListOf<Long>()
        val ok = castChannelVote({ 1_000_000L }) { at ->
            ats += at
            if (ats.size == 1) ChannelVoteAnswer.Stale(1_000_500L) else ChannelVoteAnswer.Stored
        }
        assertTrue(ok)
        assertEquals(listOf(1_000_000L, 1_000_501L), ats)
    }

    @Test
    fun `a stale vote outside the window is given up`() = runBlocking {
        var writes = 0
        val ok = castChannelVote({ 1_000_000L }) { _ ->
            writes += 1
            ChannelVoteAnswer.Stale(1_000_000L + VOTE_WINDOW_MS + 5)
        }
        assertFalse(ok)
        assertEquals(1, writes)
    }

    @Test
    fun `a clock refusal is retried at the server time`() = runBlocking {
        val ats = mutableListOf<Long>()
        castChannelVote({ 5L }) { at ->
            ats += at
            if (ats.size == 1) ChannelVoteAnswer.OutOfWindow(9_000_000L) else ChannelVoteAnswer.Stored
        }
        assertEquals(listOf(5L, 9_000_000L), ats)
    }

    @Test
    fun `a refusal and a loop of conflicts both stop`() = runBlocking {
        var n = 0
        assertFalse(castChannelVote({ 0L }) { n++; ChannelVoteAnswer.Refused(429, "vote-rate-limited") })
        assertEquals(1, n)
        n = 0
        assertFalse(castChannelVote({ 0L }) { at -> n++; ChannelVoteAnswer.Stale(at) })
        assertEquals(VOTE_MAX_ATTEMPTS, n)
    }

    @Test
    fun `the service's answers are read from status and code`() {
        assertEquals(ChannelVoteAnswer.Stored, voteAnswerOf(201, null))
        assertEquals(ChannelVoteAnswer.Stored, voteAnswerOf(204, ""))
        assertEquals(ChannelVoteAnswer.Stale(42), voteAnswerOf(409, """{"code":"stale-vote","detail":"x","stored_at_ms":42}"""))
        assertEquals(ChannelVoteAnswer.OutOfWindow(7), voteAnswerOf(400, """{"code":"vote-time-out-of-window","server_time_ms":7}"""))
        assertEquals(ChannelVoteAnswer.Refused(404, "unknown-post"), voteAnswerOf(404, """{"code":"unknown-post"}"""))
        assertEquals(ChannelVoteAnswer.Refused(409, "stale-vote"), voteAnswerOf(409, """{"code":"stale-vote"}"""))
    }
}
