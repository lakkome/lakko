// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.io.IOException
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * G-37 — host-side tests for [runSplitPoll], the split-poll ordering that keeps
 * a 30-second DS long poll out of the repository-wide `groupsLock`.
 *
 * `ChatRepository` itself needs an Android `Context` and live FFI handles, so the
 * ordering it performs is extracted into [runSplitPoll] — a pure orchestration
 * over injected steps, the same seam `runCrashSafeCommit` uses for the ADR-0040
 * commit ordering. These tests drive the REAL production function, not a
 * re-implementation of it.
 *
 * Properties pinned here:
 *
 *  1. The fetch (the long wait) does NOT hold the lock, so a concurrent send
 *     proceeds while a chat screen is parked on its long poll. **The main fix.**
 *  2. The apply DOES hold the lock, and the cursor advance happens in the same
 *     critical section — atomic with respect to each other.
 *  3. A cursor that moved during the fetch discards the stale batch instead of
 *     applying it (and instead of rewinding the cursor).
 *  4. The per-group in-flight latch stops two concurrent polls of one group from
 *     both fetching and applying overlapping batches.
 *  5. The latch is released even when the fetch throws, so a DS error / timeout
 *     never wedges a group's polling.
 *
 * Property 1 is proven to DISCRIMINATE by [runSplitPollWithFetchInsideLock] — a
 * permanent inverted-order companion holding the PRE-G-37 shape (fetch inside the
 * lock), asserted to fail the very property the fixed version passes. Without it,
 * a future refactor that quietly moved the fetch back inside the lock could leave
 * these tests green.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class SplitPollTest {

    // -----------------------------------------------------------------
    // 1. the main win: a parked fetch does not hold the lock
    // -----------------------------------------------------------------

    @Test
    fun `G-37 — a parked long-poll fetch does NOT hold the lock`() = runTest {
        val lock = Mutex()
        val fetchStarted = CompletableDeferred<Unit>()
        val releaseFetch = CompletableDeferred<Unit>()
        var concurrentSendCompleted = false

        val poll = launch {
            runSplitPoll(
                inFlight = AtomicBoolean(false),
                lock = lock,
                readCursor = { 0L },
                fetch = {
                    fetchStarted.complete(Unit)
                    releaseFetch.await()
                },
                apply = { _, _ -> },
            )
        }
        advanceUntilIdle()
        assertTrue("the fetch must have started", fetchStarted.isCompleted)

        // The lock is FREE while the long poll is parked on the network.
        assertTrue("groupsLock must be free during the fetch", lock.tryLock())
        lock.unlock()

        // And a genuinely concurrent send completes without waiting for it.
        val send = launch { lock.withLock { concurrentSendCompleted = true } }
        advanceUntilIdle()
        assertTrue(
            "a concurrent send must complete while the poll is parked on the fetch",
            concurrentSendCompleted,
        )

        releaseFetch.complete(Unit)
        advanceUntilIdle()
        poll.join()
        send.join()
    }

    @Test
    fun `G-37 discrimination — the pre-fix shape (fetch inside the lock) BLOCKS the send`() =
        runTest {
            val lock = Mutex()
            val fetchStarted = CompletableDeferred<Unit>()
            val releaseFetch = CompletableDeferred<Unit>()
            var concurrentSendCompleted = false

            val poll = launch {
                runSplitPollWithFetchInsideLock(
                    lock = lock,
                    fetch = {
                        fetchStarted.complete(Unit)
                        releaseFetch.await()
                    },
                )
            }
            advanceUntilIdle()
            assertTrue("the fetch must have started", fetchStarted.isCompleted)

            // This is exactly the G-37 defect: the global lock is held for the
            // whole 30-second long poll.
            assertFalse("the pre-fix shape holds groupsLock across the fetch", lock.tryLock())

            val send = launch { lock.withLock { concurrentSendCompleted = true } }
            advanceUntilIdle()
            assertFalse(
                "the pre-fix shape must BLOCK the concurrent send — if this passes, " +
                    "the test above proves nothing",
                concurrentSendCompleted,
            )

            releaseFetch.complete(Unit)
            advanceUntilIdle()
            poll.join()
            send.join()
            assertTrue("the send unblocks once the poll releases", concurrentSendCompleted)
        }

    // -----------------------------------------------------------------
    // 2. the apply + cursor advance are inside the lock, together
    // -----------------------------------------------------------------

    @Test
    fun `the apply runs under the lock and sees the cursor the batch was fetched with`() =
        runTest {
            val lock = Mutex()
            var cursor = 7L
            var lockHeldDuringApply = false
            var appliedAfter = -1L
            var appliedBatch = ""

            val outcome = runSplitPoll(
                inFlight = AtomicBoolean(false),
                lock = lock,
                readCursor = { cursor },
                fetch = { after -> "batch@$after" },
                apply = { after, fetched ->
                    lockHeldDuringApply = lock.isLocked
                    appliedAfter = after
                    appliedBatch = fetched
                    // the cursor advance — same critical section as the apply
                    cursor = 11L
                },
            )

            assertEquals(SplitPollOutcome.APPLIED, outcome)
            assertTrue("the apply must run while groupsLock is held", lockHeldDuringApply)
            assertEquals("the apply sees the pre-fetch cursor", 7L, appliedAfter)
            assertEquals("the apply receives the fetched batch", "batch@7", appliedBatch)
            assertEquals("the cursor advanced inside the same lock", 11L, cursor)
            assertFalse("the lock is released afterwards", lock.isLocked)
        }

    // -----------------------------------------------------------------
    // 3. a cursor that moved under us discards the stale batch
    // -----------------------------------------------------------------

    @Test
    fun `a cursor that moved during the fetch discards the batch without applying`() = runTest {
        val lock = Mutex()
        var cursor = 5L
        var applyCalls = 0

        val outcome = runSplitPoll(
            inFlight = AtomicBoolean(false),
            lock = lock,
            readCursor = { cursor },
            fetch = {
                // somebody else advanced the journal cursor while we were on the
                // network — our batch starts at a row already consumed.
                cursor = 9L
                "stale"
            },
            apply = { _, _ -> applyCalls++ },
        )

        assertEquals(SplitPollOutcome.SKIPPED_CURSOR_MOVED, outcome)
        assertEquals("a stale batch is never applied", 0, applyCalls)
        assertEquals("and never rewinds the cursor", 9L, cursor)
    }

    // -----------------------------------------------------------------
    // 4. the per-group in-flight latch
    // -----------------------------------------------------------------

    @Test
    fun `a second concurrent poll of the same group skips without fetching`() = runTest {
        val lock = Mutex()
        val inFlight = AtomicBoolean(false)
        val firstFetchStarted = CompletableDeferred<Unit>()
        val releaseFirstFetch = CompletableDeferred<Unit>()
        var fetchCalls = 0
        var applyCalls = 0

        val first = launch {
            runSplitPoll(
                inFlight = inFlight,
                lock = lock,
                readCursor = { 0L },
                fetch = {
                    fetchCalls++
                    firstFetchStarted.complete(Unit)
                    releaseFirstFetch.await()
                },
                apply = { _, _ -> applyCalls++ },
            )
        }
        advanceUntilIdle()
        assertTrue(firstFetchStarted.isCompleted)

        val second = runSplitPoll(
            inFlight = inFlight,
            lock = lock,
            readCursor = { 0L },
            fetch = { fetchCalls++ },
            apply = { _, _ -> applyCalls++ },
        )

        assertEquals(SplitPollOutcome.SKIPPED_IN_FLIGHT, second)
        assertEquals("the loser must not even hit the network", 1, fetchCalls)
        assertEquals("and must not apply", 0, applyCalls)

        releaseFirstFetch.complete(Unit)
        advanceUntilIdle()
        first.join()
        assertEquals("the winner still applies its batch", 1, applyCalls)
        assertFalse("the latch is cleared afterwards", inFlight.get())
    }

    @Test
    fun `a fetch failure releases the in-flight latch`() = runTest {
        val inFlight = AtomicBoolean(false)

        try {
            runSplitPoll(
                inFlight = inFlight,
                lock = Mutex(),
                readCursor = { 0L },
                fetch = { throw IOException("DS unreachable") },
                apply = { _: Long, _: Unit -> },
            )
        } catch (_: IOException) {
            // expected — the caller handles / backs off
        }

        assertFalse("a failed fetch must not wedge the group's polling", inFlight.get())

        // and the next poll runs normally
        var applied = false
        val outcome = runSplitPoll(
            inFlight = inFlight,
            lock = Mutex(),
            readCursor = { 0L },
            fetch = { },
            apply = { _, _ -> applied = true },
        )
        assertEquals(SplitPollOutcome.APPLIED, outcome)
        assertTrue(applied)
    }

    // -----------------------------------------------------------------
    // The permanent inverted-order companion (the ADR-0044 C-61 pattern).
    // -----------------------------------------------------------------

    /**
     * The PRE-G-37 shape, kept permanently so the discrimination test above keeps
     * discriminating: the fetch is awaited INSIDE the lock, which is exactly what
     * made an open chat screen hold the one repository-wide mutex for the whole
     * 30-second long poll. Test-only — production uses [runSplitPoll].
     */
    private suspend fun <T> runSplitPollWithFetchInsideLock(
        lock: Mutex,
        fetch: suspend (after: Long) -> T,
    ) {
        lock.withLock {
            fetch(0L)
        }
    }
}
