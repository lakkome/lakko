// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.io.IOException
import java.net.UnknownHostException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsNetworkError
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/connection-state.md` section 7 — a message that could not leave
 * because of the network waits and goes by itself when the servers answer
 * again: in order, one at a time, at most [MAX_AUTO_RESENDS] automatic sends,
 * new messages behind the waiting ones.
 *
 * The rows are a fake of the repository's (`GroupRecord`): a list of local
 * ids and their acks, changed by the injected send exactly as
 * `resendWaitingOnMlsThread` changes them.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class WaitingSendsTest {

    private class FakeConversation(private val scope: kotlinx.coroutines.CoroutineScope) {
        /** localId -> ack, in creation order (local ids count down). */
        val rows = LinkedHashMap<Long, Ack>()
        private var nextLocal = -1L
        val sent = ArrayList<Long>()
        val tried = ArrayList<Long>()
        /** What the network does to the next sends. */
        var network: (Long) -> ResendOutcome = { ResendOutcome.SENT }
        val phase = MutableStateFlow(false)

        val passes = WaitingSendPasses(
            scope = scope,
            waitingRows = { _ ->
                waitingInSendOrder(rows.map { (id, ack) -> message(id, ack) })
            },
            resend = { _, id, _ ->
                rows[id] = Ack.PENDING
                tried += id
                val outcome = network(id)
                rows[id] = when (outcome) {
                    ResendOutcome.SENT -> Ack.SENT.also { sent += id }
                    ResendOutcome.NETWORK -> Ack.WAITING
                    ResendOutcome.REFUSED -> Ack.FAILED
                    ResendOutcome.GONE -> rows.getValue(id)
                }
                outcome
            },
            markFailed = { _, id -> rows[id] = Ack.FAILED },
            isOnline = { phase.value },
            awaitOnline = { phase.first { it } },
        )

        fun newRow(ack: Ack): Long {
            val id = nextLocal
            nextLocal -= 1
            rows[id] = ack
            return id
        }

        /** What `sendMessageOnMlsThread` does with a new message. */
        fun sendNew(directOutcome: ResendOutcome): Long {
            val id = newRow(Ack.PENDING)
            if (passes.queueBehindIfWaiting(GROUP) { rows[id] = Ack.WAITING }) return id
            tried += id
            when (directOutcome) {
                ResendOutcome.SENT -> {
                    rows[id] = Ack.SENT
                    sent += id
                }
                ResendOutcome.NETWORK -> {
                    rows[id] = Ack.WAITING
                    passes.waitForOnline(GROUP)
                }
                else -> rows[id] = Ack.FAILED
            }
            return id
        }
    }

    @Test
    fun `a failure is waiting only when it was the network, through any wrapper`() {
        assertEquals(SendFailureKind.WAITING, classifySendFailure(IOException("reset")))
        assertEquals(
            SendFailureKind.WAITING,
            classifySendFailure(ChatError("send failed", DsNetworkError("/ds/group/x/messages", UnknownHostException("h")))),
        )
        // The server answered and refused: not the network.
        assertEquals(
            SendFailureKind.FAILED,
            classifySendFailure(ChatError("send failed", DsHttpError(403, "/ds/group/x/messages", "account-suspended"))),
        )
        // The suspension gate refuses before anything leaves the device.
        assertEquals(
            SendFailureKind.FAILED,
            classifySendFailure(ChatError("send failed", me.lakko.app.auth.SuspendedWriteRefused())),
        )
        assertEquals(SendFailureKind.FAILED, classifySendFailure(IllegalStateException("conversation behind")))
        assertEquals(SendFailureKind.FAILED, classifySendFailure(CancellationException("left")))
    }

    @Test
    fun `an automatic send is bounded at five network failures`() {
        for (n in 1 until MAX_AUTO_RESENDS) {
            assertEquals(Ack.WAITING, ackAfterAutoFailure(SendFailureKind.WAITING, n))
        }
        assertEquals(Ack.FAILED, ackAfterAutoFailure(SendFailureKind.WAITING, MAX_AUTO_RESENDS))
        assertEquals(Ack.FAILED, ackAfterAutoFailure(SendFailureKind.FAILED, 1))
    }

    @Test
    fun `waiting rows go oldest first and rows in flight are never listed`() {
        val rows = listOf(
            message(-1, Ack.WAITING),
            message(-2, Ack.PENDING),
            message(-3, Ack.WAITING),
            message(-4, Ack.FAILED),
            message(-5, Ack.WAITING),
        )
        assertEquals(listOf(-1L, -3L, -5L), waitingInSendOrder(rows))
    }

    @Test
    fun `two network-failed messages are each sent exactly once, oldest first, when the state goes online`() = runTest {
        val conv = FakeConversation(detachedScope())
        val first = conv.sendNew(ResendOutcome.NETWORK)
        val second = conv.sendNew(ResendOutcome.SENT)
        advanceUntilIdle()
        // The second was never tried: it queued behind the first.
        assertEquals(Ack.WAITING, conv.rows[second])
        assertEquals(listOf(first), conv.tried)
        assertTrue(conv.sent.isEmpty())

        conv.phase.value = true
        advanceUntilIdle()
        assertEquals(listOf(first, second), conv.sent)
        assertEquals(listOf(first, first, second), conv.tried)
        assertEquals(Ack.SENT, conv.rows[first])
        assertEquals(Ack.SENT, conv.rows[second])

        // Going offline and online again sends nothing twice.
        conv.phase.value = false
        runCurrent()
        conv.phase.value = true
        advanceUntilIdle()
        assertEquals(listOf(first, second), conv.sent)
    }

    @Test
    fun `a pass stops at a network failure without spending the later rows' attempts`() = runTest {
        val conv = FakeConversation(detachedScope())
        val a = conv.newRow(Ack.WAITING)
        val b = conv.newRow(Ack.WAITING)
        val c = conv.newRow(Ack.WAITING)
        conv.network = { id -> if (id == b) ResendOutcome.NETWORK else ResendOutcome.SENT }
        conv.phase.value = true
        conv.passes.request(GROUP)
        runCurrent()
        assertEquals(listOf(a, b), conv.tried)
        assertEquals(Ack.WAITING, conv.rows[b])
        assertEquals(Ack.WAITING, conv.rows[c])
        assertEquals(1, conv.passes.networkFailuresOf(GROUP, b))
        assertEquals(0, conv.passes.networkFailuresOf(GROUP, c))
    }

    @Test
    fun `a refused message is not sent by itself`() = runTest {
        val conv = FakeConversation(detachedScope())
        val refused = conv.sendNew(ResendOutcome.REFUSED)
        assertEquals(Ack.FAILED, conv.rows[refused])
        conv.phase.value = true
        advanceUntilIdle()
        assertEquals(listOf(refused), conv.tried)
        assertTrue(conv.sent.isEmpty())
        assertFalse(conv.passes.isRunning(GROUP))
    }

    @Test
    fun `after five automatic network failures the message reads Not sent`() = runTest {
        val conv = FakeConversation(detachedScope())
        val id = conv.sendNew(ResendOutcome.NETWORK)
        conv.network = { ResendOutcome.NETWORK }
        // The servers answer other requests while this send keeps getting
        // no answer: it is tried again a few seconds after each failure.
        conv.phase.value = true
        advanceUntilIdle()
        assertEquals(Ack.FAILED, conv.rows[id])
        // The first send was the person's; five more were automatic.
        assertEquals(1 + MAX_AUTO_RESENDS, conv.tried.size)
        // Nothing sends it again by itself.
        conv.phase.value = false
        runCurrent()
        conv.phase.value = true
        advanceUntilIdle()
        assertEquals(1 + MAX_AUTO_RESENDS, conv.tried.size)
    }

    @Test
    fun `a new message queues behind waiting ones and goes in turn`() = runTest {
        val conv = FakeConversation(detachedScope())
        val waiting = conv.newRow(Ack.WAITING)
        conv.phase.value = true
        val later = conv.sendNew(ResendOutcome.SENT)
        assertEquals(Ack.WAITING, conv.rows[later])
        advanceUntilIdle()
        assertEquals(listOf(waiting, later), conv.sent)
    }

    @Test
    fun `a tap on a waiting row starts the pass now and is not counted as automatic`() = runTest {
        val conv = FakeConversation(detachedScope())
        val id = conv.newRow(Ack.WAITING)
        conv.network = { ResendOutcome.NETWORK }
        conv.passes.request(GROUP, tappedLocalId = id)
        runCurrent()
        assertEquals(listOf(id), conv.tried)
        assertEquals(0, conv.passes.networkFailuresOf(GROUP, id))
        assertEquals(Ack.WAITING, conv.rows[id])
    }

    private companion object {
        const val GROUP = "ab12"

        fun message(localId: Long, ack: Ack) = ChatMessage(
            messageId = 0,
            direction = Direction.Outgoing,
            text = "m$localId",
            createdAt = "2026-11-24T10:00:00Z",
            ack = ack,
            localId = localId,
        )
    }
}

/** The test's scheduler, but not a child of the test: `advanceUntilIdle`
 *  runs its work (it skips `backgroundScope`), and a waiter still parked at
 *  the end does not hold the test open. */
private fun kotlinx.coroutines.test.TestScope.detachedScope(): kotlinx.coroutines.CoroutineScope =
    kotlinx.coroutines.CoroutineScope(
        coroutineContext.minusKey(kotlinx.coroutines.Job) + kotlinx.coroutines.SupervisorJob(),
    )
