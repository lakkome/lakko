// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** UX `reporting-and-moderation.md` 1.4 — the block's send to the other devices is bounded, retried and never awaited. */
class OwedSendTest {

    @Test
    fun `stops at the first attempt that goes`() = runTest {
        val seen = mutableListOf<Int>()
        val went = sendOwed(attempt = { i -> seen += i; i == 1 }, retryDelaysMs = listOf(10L, 20L))
        assertTrue(went)
        assertEquals(listOf(0, 1), seen)
    }

    @Test
    fun `a throw is a failure, retried after each delay, and stays owed when none goes`() = runTest {
        val seen = mutableListOf<Int>()
        var failures = 0
        val went = sendOwed(
            attempt = { i -> seen += i; error("offline") },
            retryDelaysMs = listOf(30L, 120L),
            onFailure = { failures += 1 },
        )
        assertFalse(went)
        assertEquals(listOf(0, 1, 2), seen)
        assertEquals(3, failures)
    }

    @Test
    fun `an attempt that hangs counts as a failure once its time is up`() = runTest {
        val never = CompletableDeferred<Boolean>()
        var failures = 0
        var result: Boolean? = null
        val job = launch {
            result = sendOwed(
                attempt = { never.await() },
                attemptMs = 20_000L,
                retryDelaysMs = emptyList(),
                onFailure = { failures += 1 },
            )
        }
        advanceTimeBy(19_999L)
        runCurrent()
        assertEquals(null, result)
        advanceTimeBy(2L)
        runCurrent()
        job.join()
        assertEquals(false, result)
        assertEquals(1, failures)
    }

    @Test
    fun `stops once the work no longer belongs to the signed-in account`() = runTest {
        var current = true
        val seen = mutableListOf<Int>()
        val went = sendOwed(
            attempt = { i -> seen += i; current = false; false },
            retryDelaysMs = listOf(10L, 20L),
            isCurrent = { current },
        )
        assertFalse(went)
        assertEquals(listOf(0), seen)
    }

    @Test
    fun `the caller is not held while the send hangs`() = runTest {
        val never = CompletableDeferred<Boolean>()
        // As the sync runtime does it: fold, launch the send, return.
        var callerDone = false
        val launched = launch { sendOwed(attempt = { never.await() }) }
        callerDone = true
        runCurrent()
        assertTrue(callerDone)
        // The send is still out in the background; the caller went on.
        assertFalse(launched.isCompleted)
        launched.cancel()
    }
}
