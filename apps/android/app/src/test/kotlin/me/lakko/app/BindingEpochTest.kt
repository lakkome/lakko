// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * [BindingEpoch] answers "is the account this work was started for still the
 * bound one?". These pin both ways work uses it: a captured [Binding] asked
 * after a suspension, and a coroutine launched with [launchBound] that is
 * cancelled with the binding.
 */
class BindingEpochTest {

    @Test
    fun `a captured binding is current until the binding ends`() {
        val epoch = BindingEpoch()
        val bound = epoch.capture()
        assertTrue(bound.isCurrent)
        bound.ensureCurrent()

        epoch.advance()
        assertFalse(bound.isCurrent)
    }

    @Test
    fun `signing the same account in again is a new binding`() {
        val epoch = BindingEpoch()
        val first = epoch.capture()
        epoch.advance() // signed out
        val second = epoch.capture() // the same account signed in again
        assertFalse(first.isCurrent)
        assertTrue(second.isCurrent)

        epoch.advance()
        assertFalse(second.isCurrent)
        // An old capture never becomes current again.
        assertFalse(first.isCurrent)
    }

    @Test
    fun `a stale binding stops the work as a cancellation`() {
        val epoch = BindingEpoch()
        val bound = epoch.capture()
        epoch.advance()
        try {
            bound.ensureCurrent()
            fail("a stale binding passed")
        } catch (stale: StaleBindingException) {
            // A cancellation: reporting catch sites pass it on unreported.
            assertTrue((stale as Throwable) is CancellationException)
        }
    }

    @Test
    fun `a reporting catch passes a stale binding on without reporting it`() = runBlocking {
        val epoch = BindingEpoch()
        val bound = epoch.capture()
        epoch.advance()
        var reported: Throwable? = null
        var released = false
        try {
            try {
                bound.ensureCurrent()
            } catch (e: Throwable) {
                rethrowIfCancelled(e) { released = true }
                reported = e
            }
            fail("the stale binding was swallowed")
        } catch (_: StaleBindingException) {
            // expected
        }
        assertNull(reported)
        assertTrue(released)
    }

    @Test
    fun `work launched for a binding never runs the code after its suspension once the binding ended`() =
        runBlocking {
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
            val epoch = BindingEpoch()
            val started = CompletableDeferred<Unit>()
            val response = CompletableDeferred<String>()
            var wrote: String? = null

            val job = scope.launchBound(epoch) {
                started.complete(Unit)
                // The request is out when the account signs out.
                val answer = response.await()
                wrote = answer
            }
            withTimeout(5_000) { started.await() }

            epoch.advance()
            // The response arrives afterwards.
            response.complete("the first account's answer")
            withTimeout(5_000) { job.join() }

            assertTrue(job.isCancelled)
            assertNull(wrote)
            // The scope itself is untouched: only the binding's work ended.
            assertTrue(scope.isActive)
            scope.cancel()
        }

    @Test
    fun `work launched after the binding ended belongs to the next binding`() = runBlocking {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val epoch = BindingEpoch()
        epoch.advance()
        var ran = false
        val job = scope.launchBound(epoch) { ran = true }
        withTimeout(5_000) { job.join() }
        assertTrue(ran)
        assertFalse(job.isCancelled)
        scope.cancel()
    }

    @Test
    fun `one failed piece of bound work does not end its siblings`() = runBlocking {
        val scope = CoroutineScope(
            SupervisorJob() + Dispatchers.Default +
                kotlinx.coroutines.CoroutineExceptionHandler { _, _ -> },
        )
        val epoch = BindingEpoch()
        val gate = CompletableDeferred<Unit>()
        var siblingRan = false
        val failing = scope.launchBound(epoch) { throw IllegalStateException("one leg failed") }
        val sibling = scope.launchBound(epoch) {
            gate.await()
            siblingRan = true
        }
        withTimeout(5_000) { failing.join() }
        gate.complete(Unit)
        withTimeout(5_000) { sibling.join() }
        assertTrue(siblingRan)
        scope.cancel()
    }

    @Test
    fun `an atomically started coroutine still wipes what it owns when its binding is already gone`() =
        runBlocking {
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
            val epoch = BindingEpoch()
            val seed = ByteArray(32) { 7 }
            val stale = epoch.job()
            epoch.advance()
            // A launch that lost the race with the sign-out: its parent is the
            // binding that just ended.
            var reachedTheWrite = false
            val job = scope.launchAtomicUnder(stale) {
                try {
                    kotlinx.coroutines.yield()
                    reachedTheWrite = true
                } finally {
                    seed.fill(0)
                }
            }
            withTimeout(5_000) { job.join() }
            assertFalse(reachedTheWrite)
            assertEquals(List(32) { 0.toByte() }, seed.toList())
            scope.cancel()
        }

    private fun CoroutineScope.launchAtomicUnder(
        parent: kotlinx.coroutines.Job,
        block: suspend CoroutineScope.() -> Unit,
    ): kotlinx.coroutines.Job =
        launch(context = parent, start = CoroutineStart.ATOMIC, block = block)

    @Test
    fun `a stale binding out of one pass does not end the loop that made the call`() = runBlocking {
        var passes = 0
        val loop = launch {
            while (passes < 3) {
                try {
                    passes += 1
                    // The repository's session ended under this pass.
                    if (passes == 1) throw StaleBindingException()
                } catch (cancel: CancellationException) {
                    endPassUnlessCancelled(cancel)
                }
            }
        }
        withTimeout(5_000) { loop.join() }
        assertEquals(3, passes)
        assertFalse(loop.isCancelled)
    }

    @Test
    fun `any other cancellation still ends the loop`() = runBlocking {
        try {
            endPassUnlessCancelled(CancellationException("the screen went away"))
            fail("a cancellation that is not a stale binding was swallowed")
        } catch (cause: CancellationException) {
            assertFalse(cause is StaleBindingException)
        }
    }

    @Test
    fun `a stale binding ends the loop when the loop itself is cancelled too`() = runBlocking {
        var wentOn = false
        val loop = launch {
            // Cancelled, and still running up to its next suspension.
            coroutineContext.cancel()
            try {
                endPassUnlessCancelled(StaleBindingException())
                wentOn = true
            } catch (_: CancellationException) {
                // expected
            }
        }
        withTimeout(5_000) { loop.join() }
        assertFalse(wentOn)
    }

    @Test
    fun `of the work that captured one binding exactly one takes it over`() {
        val epoch = BindingEpoch()
        val first = epoch.capture()
        val second = epoch.capture()

        first.takeOver()

        // The binding ended with the take-over: the other sign-in is dropped,
        // and so is a second attempt by the first.
        assertFalse(second.isCurrent)
        for (late in listOf(second, first)) {
            try {
                late.takeOver()
                fail("a binding was taken over twice")
            } catch (_: StaleBindingException) {
                // expected
            }
        }
        // What is captured now is the binding the winner lands into.
        assertTrue(epoch.capture().isCurrent)
    }

    @Test
    fun `a binding that already ended cannot be taken over`() {
        val epoch = BindingEpoch()
        val bound = epoch.capture()
        epoch.advance()
        try {
            bound.takeOver()
            fail("a sign-in that outlived a sign-out took the binding over")
        } catch (_: StaleBindingException) {
            // expected
        }
    }

    @Test
    fun `taking a binding over ends the work launched for it`() = runBlocking {
        val epoch = BindingEpoch()
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val started = CompletableDeferred<Unit>()
        val job = scope.launchBound(epoch) {
            started.complete(Unit)
            CompletableDeferred<Unit>().await()
        }
        withTimeout(5_000) { started.await() }

        epoch.capture().takeOver()

        withTimeout(5_000) { job.join() }
        assertTrue(job.isCancelled)
        scope.cancel()
    }
}
