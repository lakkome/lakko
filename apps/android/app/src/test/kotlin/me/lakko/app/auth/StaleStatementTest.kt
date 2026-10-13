// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import java.io.IOException
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * ADR-0074 "T5 as built" items 17, 24 and 25: a stale account statement is
 * signed again at the server's time, once, and never read as a refusal; a
 * `recall/finish` whose answer was lost is asked again with the same bytes.
 */
class StaleStatementTest {

    private fun stale(serverTimeMs: Long? = 1_700_000_000_000L): AuthHttpError {
        val body = if (serverTimeMs == null) {
            """{"error":"statement-stale"}"""
        } else {
            """{"error":"statement-stale","server_time_ms":$serverTimeMs}"""
        }
        return AuthHttpError(401, "/account/recall/status", body, code = "statement-stale")
    }

    private fun http(status: Int, code: String?) = AuthHttpError(status, "/x", "{}", code = code)

    private fun offline() = AuthNetworkError("/x", IOException("no route"))

    @Test
    fun `the server's time is read only from a statement-stale answer`() {
        assertEquals(1_700_000_000_000L, statementStaleServerTimeMs(stale()))
        // No usable time: nothing to sign at.
        assertNull(statementStaleServerTimeMs(stale(serverTimeMs = null)))
        assertNull(statementStaleServerTimeMs(stale(serverTimeMs = 0)))
        // Any other answer is not it.
        assertNull(statementStaleServerTimeMs(http(401, "recall-refused")))
        assertNull(statementStaleServerTimeMs(http(401, "key-generation-superseded")))
        assertNull(statementStaleServerTimeMs(http(403, "statement-stale")))
        assertNull(statementStaleServerTimeMs(offline()))
    }

    @Test
    fun `a stale statement is signed again at the server's time and sent once more`() = runTest {
        val signedAt = mutableListOf<Long?>()
        var monotonic = 1_000L
        val answer = sendWithStaleStatementRetry(monotonicMs = { monotonic }) { signAtMs ->
            signedAt += signAtMs
            if (signAtMs == null) {
                // 250 ms pass between reading the answer and signing again.
                throw stale().also { monotonic += 0 }
            }
            "ok"
        }
        assertEquals("ok", answer)
        assertEquals(listOf<Long?>(null, 1_700_000_000_000L), signedAt)
    }

    @Test
    fun `the milliseconds that passed since the answer was read are added`() = runTest {
        val clock = ArrayDeque(listOf(5_000L, 5_250L))
        val signedAt = mutableListOf<Long?>()
        sendWithStaleStatementRetry(monotonicMs = { clock.removeFirst() }) { signAtMs ->
            signedAt += signAtMs
            if (signAtMs == null) throw stale()
        }
        assertEquals(1_700_000_000_250L, signedAt.last())
    }

    @Test
    fun `a second stale answer is not retried again`() = runTest {
        var sends = 0
        try {
            sendWithStaleStatementRetry<Unit> {
                sends += 1
                throw stale()
            }
            fail("the second stale answer must propagate")
        } catch (err: AuthHttpError) {
            assertEquals(STATEMENT_STALE, err.code)
        }
        assertEquals(2, sends)
    }

    @Test
    fun `any other failure is not retried`() = runTest {
        var sends = 0
        val refused = http(401, "recall-refused")
        try {
            sendWithStaleStatementRetry<Unit> {
                sends += 1
                throw refused
            }
            fail("must propagate")
        } catch (err: AuthHttpError) {
            assertSame(refused, err)
        }
        assertEquals(1, sends)
    }

    @Test
    fun `a stale statement never destroys the recall record`() {
        // Only the exact `recall-refused` ends a recall; a stale answer that
        // survived its one retry is an ordinary failure.
        assertEquals(RecallFailure.Other, recallFailureOf(stale()))
        assertFalse(recallFailureOf(stale()) == RecallFailure.Refused)
        assertTrue(recallStatusOutcomeOf(stale()) is RecallStatusOutcome.Later)
    }

    @Test
    fun `a stale upgrade statement is never read as superseded`() {
        // A kept device with a wrong clock must not be told "reset".
        assertTrue(upgradeOutcomeOf(stale()) is UpgradeOutcome.Unanswered)
        assertTrue(upgradeOutcomeOf(stale(serverTimeMs = null)) is UpgradeOutcome.Unanswered)
    }

    // --- recall/finish: a lost answer --------------------------------------

    @Test
    fun `a lost answer is a network failure or a gateway fault, never a real answer`() {
        assertTrue(recallFinishAnswerLost(offline()))
        assertTrue(recallFinishAnswerLost(http(502, null)))
        assertTrue(recallFinishAnswerLost(http(503, null)))
        assertTrue(recallFinishAnswerLost(http(504, null)))
        assertFalse(recallFinishAnswerLost(http(401, "recall-refused")))
        assertFalse(recallFinishAnswerLost(http(401, "wrong-password")))
        assertFalse(recallFinishAnswerLost(http(429, "too-many-attempts")))
        assertFalse(recallFinishAnswerLost(http(500, null)))
        assertFalse(recallFinishAnswerLost(RecallWrongPassword()))
    }

    @Test
    fun `the same finish is sent again while the answer is lost, then handed back`() {
        val sent = 1_000_000L
        assertEquals(1_000L, recallFinishRetryDelayMs(offline(), repeatsMade = 0, firstSentAtMs = sent, nowMs = sent))
        assertEquals(3_000L, recallFinishRetryDelayMs(offline(), 1, sent, sent + 1_500))
        assertEquals(8_000L, recallFinishRetryDelayMs(offline(), 2, sent, sent + 5_000))
        // The in-call repeats are used up: the caller keeps the request.
        assertNull(recallFinishRetryDelayMs(offline(), 3, sent, sent + 13_000))
        // A real answer is never repeated.
        assertNull(recallFinishRetryDelayMs(http(401, "recall-refused"), 0, sent, sent))
        // Past the server's replay window nothing is asked again.
        assertNull(recallFinishRetryDelayMs(offline(), 0, sent, sent + RECALL_FINISH_REPLAY_WINDOW_MS))
    }

    @Test
    fun `a finish can be asked again for ten minutes and not after a clock step back`() {
        val sent = 5_000_000L
        assertTrue(recallFinishReplayable(sent, sent))
        assertTrue(recallFinishReplayable(sent, sent + RECALL_FINISH_REPLAY_WINDOW_MS - 1))
        assertFalse(recallFinishReplayable(sent, sent + RECALL_FINISH_REPLAY_WINDOW_MS))
        assertFalse(recallFinishReplayable(sent, sent - 1))
    }

    @Test
    fun `an unanswered finish keeps its request bytes and leaves the record alone`() {
        val request = RecallFinishRequest(
            session_id = "s",
            account_id_b64 = "a",
            anchor_seq = 3,
            login_finish = "lf",
            password_upload = "pu",
            recovery_upload = "ru",
            new_signature_pubkey_b64 = "k",
            device_id_b64 = null,
            statement_b64 = "st",
        )
        val unanswered = RecallFinishUnanswered(request, firstSentAtMs = 7, cause = offline())
        // The very same request object is what gets sent again.
        assertSame(request, unanswered.request)
        assertEquals(7, unanswered.firstSentAtMs)
        // The form shows "offline"; the recall record is not destroyed.
        assertEquals(RecallFailure.Offline, recallFailureOf(unanswered))
    }
}
