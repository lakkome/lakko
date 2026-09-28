// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0054 S4/S5 - the signup solve runs off the caller's thread as a loop of
 * short native chunks; a cancel returns at once AND stops the loop at the next
 * chunk boundary, so the CPU work ends with the screen.
 */
class PowSolveTest {

    @Test
    fun aSolveLoopsChunksOnItsOwnThreadUntilOneFindsAProof() = runBlocking {
        val caller = Thread.currentThread()
        var solverThread: Thread? = null
        val chunks = AtomicInteger()
        val budgets = mutableListOf<Int>()
        val proof = solvePow("c2VlZA==", 8, solver = { seed, bits, max ->
            solverThread = Thread.currentThread()
            budgets += max
            if (chunks.incrementAndGet() < 3) null else PowProofPayload(seed, "n$bits", "s")
        })
        assertEquals(PowProofPayload("c2VlZA==", "n8", "s"), proof)
        assertTrue(solverThread !== caller)
        assertEquals(3, chunks.get())
        assertEquals(List(3) { POW_CHUNK_ATTEMPTS }, budgets)
    }

    @Test
    fun aSolverErrorEndsTheSolve() = runBlocking {
        val err = runCatching {
            solvePow("seed", 8, solver = { _, _, _ -> throw IllegalStateException("bad seed") })
        }.exceptionOrNull()
        assertTrue(err is IllegalStateException)
    }

    @Test
    fun cancellingReturnsAtOnceAndStopsTheChunkLoop() = runBlocking {
        val chunks = AtomicInteger()
        val inThird = CountDownLatch(1)
        val release = CountDownLatch(1)
        val executor = Executors.newSingleThreadExecutor()
        try {
            val solve = async(Dispatchers.Default) {
                solvePow("seed", 8, solver = { _, _, _ ->
                    if (chunks.incrementAndGet() == 3) {
                        inThird.countDown()
                        release.await(10, TimeUnit.SECONDS)
                    }
                    null
                }, executor = executor)
            }
            assertTrue(inThird.await(5, TimeUnit.SECONDS))
            // The third chunk is blocked; the cancel must not wait for it.
            withTimeout(2_000) {
                solve.cancel()
                val outcome = runCatching { solve.await() }.exceptionOrNull()
                assertTrue(outcome is CancellationException)
            }
            release.countDown()
            // The loop runs on this single thread: once a task queued behind
            // it runs, the loop has returned.
            val drained = CountDownLatch(1)
            executor.execute { drained.countDown() }
            assertTrue("the chunk loop kept running after the cancel", drained.await(5, TimeUnit.SECONDS))
            assertEquals("no chunk started after the cancel", 3, chunks.get())
        } finally {
            executor.shutdownNow()
        }
    }
}
