// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.io.IOException
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.currentTime
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.flow.first
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsNetworkError
import me.lakko.app.transport.backoffAfterFailure
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/connection-state.md` section 3 — when the network comes back every
 * receive loop retries at once instead of finishing its backoff (the open
 * conversation's loop, the app-wide sweep's pause, the DeviceGroup loop).
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ReceiveLoopNetworkReturnTest {

    @Test
    fun `the chat screen loop polls again as soon as the network returns`() = runTest {
        val returned = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
        val claim = ScreenPollClaim(claim = {}, release = {})
        var polls = 0
        val job = launch {
            runScreenPollLoop(
                ended = MutableStateFlow(false),
                claim = claim,
                poll = {
                    polls += 1
                    throw IOException("no route")
                },
                // What ChatScreen passes, with the flow injected.
                backoff = { withTimeoutOrNull(5_000L) { returned.first() } },
            )
        }
        runCurrent()
        assertEquals(1, polls)
        advanceTimeBy(1_000L)
        runCurrent()
        assertEquals("still backing off", 1, polls)
        returned.tryEmit(Unit)
        runCurrent()
        assertEquals("polled again at once, 4 s before the backoff ended", 2, polls)
        job.cancel()
    }

    @Test
    fun `a network failure's backoff ends when the network returns, any other failure waits it out`() = runTest {
        val returned = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
        val start = currentTime
        val network = launch {
            backoffAfterFailure(DsNetworkError("/ds/group/x/messages", IOException("reset")), 5_000L, returned)
        }
        runCurrent()
        advanceTimeBy(500L)
        returned.tryEmit(Unit)
        runCurrent()
        assertTrue("ended early", network.isCompleted)
        assertEquals(500L, currentTime - start)

        val refused = launch {
            backoffAfterFailure(DsHttpError(503, "/ds/group/x/messages", ""), 5_000L, returned)
        }
        runCurrent()
        returned.tryEmit(Unit)
        runCurrent()
        assertTrue("an HTTP refusal is not cured by a route", refused.isActive)
        advanceTimeBy(5_001L)
        assertTrue(refused.isCompleted)
    }

    @Test
    fun `the app-wide sweep's pause ends when the network returns`() = runTest {
        val returned = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
        val waiting = launch {
            ReceiveCadence.awaitWakeOrNewGroup(
                wakeBefore = Long.MAX_VALUE,
                swept = setOf("aa"),
                groups = emptyFlow(),
                networkReturned = returned,
            )
        }
        runCurrent()
        assertTrue(waiting.isActive)
        returned.tryEmit(Unit)
        runCurrent()
        assertTrue(waiting.isCompleted)
    }
}
