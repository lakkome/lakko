// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * [rethrowIfCancelled] is the first line of every `catch (e: Throwable)` that
 * reports a failed request. These pin the two halves of its contract: a real
 * failure passes through it untouched, and a cancelled request never reaches
 * the reporting code, in whatever shape the cancellation arrives.
 */
class CancellationTest {

    /** What a handler's `catch` did, as the screen would see it. */
    private class Handler {
        var reported: Throwable? = null
        var busy: Boolean = true

        suspend fun run(request: suspend () -> Unit) {
            try {
                request()
                busy = false
            } catch (e: Throwable) {
                rethrowIfCancelled(e) { busy = false }
                reported = e
                busy = false
            }
        }
    }

    @Test
    fun `a real failure is reported exactly as before`() = runBlocking {
        val handler = Handler()
        val failure = IllegalStateException("the server said no")
        handler.run { throw failure }
        assertEquals(failure, handler.reported)
        assertFalse(handler.busy)
    }

    @Test
    fun `a timeout that ran out is a real failure`() = runBlocking {
        val handler = Handler()
        handler.run { withTimeout(1) { awaitCancellation() } }
        assertTrue(handler.reported is CancellationException)
        assertFalse(handler.busy)
    }

    @Test
    fun `a request cancelled mid-flight reports nothing and lets go of the busy flag`() = runBlocking {
        val handler = Handler()
        val started = CompletableDeferred<Unit>()
        val job = launch(Dispatchers.Default) {
            handler.run {
                started.complete(Unit)
                awaitCancellation()
            }
        }
        started.await()
        job.cancelAndJoin()
        assertNull(handler.reported)
        assertFalse(handler.busy)
        assertTrue(job.isCancelled)
    }

    @Test
    fun `a cancellation a lower layer wrapped into its own error reports nothing either`() = runBlocking {
        // The repository shape: `catch (e: Throwable) { throw ChatError(..., e) }`
        // around the network call. The handler receives an ordinary exception
        // whose only connection to the cancellation is its cause.
        val handler = Handler()
        val started = CompletableDeferred<Unit>()
        val job = launch(Dispatchers.Default) {
            handler.run {
                try {
                    started.complete(Unit)
                    awaitCancellation()
                } catch (e: CancellationException) {
                    throw IllegalStateException("publish failed", e)
                }
            }
        }
        started.await()
        job.cancelAndJoin()
        assertNull(handler.reported)
        assertFalse(handler.busy)
        assertTrue(job.isCancelled)
    }

    @Test
    fun `the release runs only on the cancelled path`() = runBlocking {
        var released = 0
        try {
            throw IllegalStateException("real")
        } catch (e: Throwable) {
            rethrowIfCancelled(e) { released += 1 }
        }
        assertEquals(0, released)

        val job = launch(start = CoroutineStart.UNDISPATCHED) {
            try {
                awaitCancellation()
            } catch (e: Throwable) {
                rethrowIfCancelled(e) { released += 1 }
            }
        }
        job.cancelAndJoin()
        assertEquals(1, released)
    }
}
