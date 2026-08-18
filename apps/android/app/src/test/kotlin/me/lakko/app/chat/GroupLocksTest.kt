// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.io.IOException
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotSame
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * G-37 sprint 2 — host-side tests for [GroupLocks] and [runPollAllGroups]: the
 * narrowing of `ChatRepository`'s single repository-wide `groupsLock` to one
 * mutex per group, and the concurrent chat-list poll that narrowing unlocks.
 *
 * `ChatRepository` needs an Android `Context` and live FFI handles, so — exactly
 * as sprint 1 did for [runSplitPoll] — the properties live in small production
 * seams these tests drive DIRECTLY. Nothing here re-implements the production
 * logic.
 *
 * Properties pinned here:
 *
 *  1. Two operations on DIFFERENT groups do not block each other. **The point of
 *     the change.**
 *  2. Two operations on the SAME group still serialise — the ADR-0040 ordering
 *     that must survive: commit A publishes before commit B is even created.
 *  3. [runPollAllGroups] polls concurrently, not serially.
 *  4. …but bounded, so it cannot saturate OkHttp's per-host dispatcher slots.
 *  5. One group's failure does not sink the rest.
 *
 * Properties 1 and 3 are proven to DISCRIMINATE by permanent pre-fix companions
 * — [SingleRepositoryLock] (one mutex for every group) and
 * [runPollAllGroupsSerially] — each asserted to FAIL the property the fixed
 * version passes. Same pattern sprint 1 used for the split poll and ADR-0044
 * used for C-61: without them a future refactor that quietly reinstated the
 * global lock, or dropped the fan-out, could leave these tests green.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class GroupLocksTest {

    // -----------------------------------------------------------------
    // 1. identity of the mutexes
    // -----------------------------------------------------------------

    @Test
    fun `the same group always resolves to the same mutex`() {
        val locks = GroupLocks()
        assertSame(locks.forGroup("aa"), locks.forGroup("aa"))
        assertEquals("one key, one entry", 1, locks.size)
    }

    @Test
    fun `different groups resolve to different mutexes`() {
        val locks = GroupLocks()
        assertNotSame(locks.forGroup("aa"), locks.forGroup("bb"))
        assertEquals(2, locks.size)
    }

    @Test
    fun `clear drops every mutex`() {
        val locks = GroupLocks()
        locks.forGroup("aa")
        locks.forGroup("bb")
        locks.clear()
        assertEquals(0, locks.size)
    }

    // -----------------------------------------------------------------
    // 2. the main win: different groups do not block each other
    // -----------------------------------------------------------------

    @Test
    fun `G-37 — an operation on one group does NOT block an operation on another`() = runTest {
        val locks = GroupLocks()
        val aHoldsLock = CompletableDeferred<Unit>()
        val releaseA = CompletableDeferred<Unit>()
        var bCompleted = false

        // Group A is parked mid-operation — think a 15 s DS publish.
        val a = launch {
            locks.forGroup("aaaa").withLock {
                aHoldsLock.complete(Unit)
                releaseA.await()
            }
        }
        advanceUntilIdle()
        assertTrue("group A must be holding its lock", aHoldsLock.isCompleted)

        // Group B proceeds regardless.
        val b = launch { locks.forGroup("bbbb").withLock { bCompleted = true } }
        advanceUntilIdle()
        assertTrue(
            "an operation on group B must complete while group A is parked on its publish",
            bCompleted,
        )

        releaseA.complete(Unit)
        advanceUntilIdle()
        a.join()
        b.join()
    }

    @Test
    fun `G-37 discrimination — one repository-wide mutex BLOCKS the other group`() = runTest {
        val locks = SingleRepositoryLock()
        val aHoldsLock = CompletableDeferred<Unit>()
        val releaseA = CompletableDeferred<Unit>()
        var bCompleted = false

        val a = launch {
            locks.forGroup("aaaa").withLock {
                aHoldsLock.complete(Unit)
                releaseA.await()
            }
        }
        advanceUntilIdle()
        assertTrue(aHoldsLock.isCompleted)

        val b = launch { locks.forGroup("bbbb").withLock { bCompleted = true } }
        advanceUntilIdle()
        assertFalse(
            "the pre-fix shape must BLOCK the other group — if this passes, the " +
                "test above proves nothing",
            bCompleted,
        )

        releaseA.complete(Unit)
        advanceUntilIdle()
        a.join()
        b.join()
        assertTrue("group B unblocks once group A releases", bCompleted)
    }

    // -----------------------------------------------------------------
    // 3. the guarantee that must survive: SAME group still serialises
    // -----------------------------------------------------------------

    @Test
    fun `G-37 — two operations on the SAME group still serialise`() = runTest {
        val locks = GroupLocks()
        val firstHoldsLock = CompletableDeferred<Unit>()
        val releaseFirst = CompletableDeferred<Unit>()
        var secondCompleted = false

        val first = launch {
            locks.forGroup("aaaa").withLock {
                firstHoldsLock.complete(Unit)
                releaseFirst.await()
            }
        }
        advanceUntilIdle()
        assertTrue(firstHoldsLock.isCompleted)

        val second = launch { locks.forGroup("aaaa").withLock { secondCompleted = true } }
        advanceUntilIdle()
        assertFalse(
            "a second commit on the SAME group must NOT start before the first " +
                "has published — ADR-0040's device-ahead strand",
            secondCompleted,
        )

        releaseFirst.complete(Unit)
        advanceUntilIdle()
        first.join()
        second.join()
        assertTrue("and it proceeds once the first releases", secondCompleted)
    }

    /**
     * The ordering ADR-0040 actually depends on, asserted on OBSERVED ORDER
     * rather than on a blocked flag: for one group, commit A's publish is
     * recorded before commit B is even CREATED.
     */
    @Test
    fun `G-37 — same-group commit A publishes before commit B is created`() = runTest {
        val locks = GroupLocks()
        val order = mutableListOf<String>()
        val aCreated = CompletableDeferred<Unit>()
        val releaseAPublish = CompletableDeferred<Unit>()

        val a = launch {
            locks.forGroup("aaaa").withLock {
                order.add("A-create")
                aCreated.complete(Unit)
                // The DS publish is a SUSPENDING network call, and it happens
                // inside the per-group lock. Suspending here is what makes this
                // test discriminate: without a shared lock B would interleave
                // during exactly this window.
                releaseAPublish.await()
                order.add("A-publish")
            }
        }
        // B only tries once A is definitely mid-operation.
        aCreated.await()
        val b = launch {
            locks.forGroup("aaaa").withLock {
                order.add("B-create")
                order.add("B-publish")
            }
        }
        // Give B every chance to interleave while A is parked on its publish.
        advanceUntilIdle()
        releaseAPublish.complete(Unit)
        advanceUntilIdle()
        a.join()
        b.join()

        assertEquals(listOf("A-create", "A-publish", "B-create", "B-publish"), order)
        assertTrue(
            "A must publish before B is created",
            order.indexOf("A-publish") < order.indexOf("B-create"),
        )
    }

    // -----------------------------------------------------------------
    // 4. pollAllGroups: concurrent, bounded, isolated
    // -----------------------------------------------------------------

    @Test
    fun `G-37 — pollAllGroups polls concurrently, not serially`() = runTest {
        val inFlight = AtomicInteger(0)
        val peak = AtomicInteger(0)
        val allStarted = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()

        val job = launch {
            runPollAllGroups(
                groupIds = listOf("a", "b", "c"),
                concurrency = 3,
            ) {
                val now = inFlight.incrementAndGet()
                peak.updateAndGet { max -> if (now > max) now else max }
                if (now == 3) allStarted.complete(Unit)
                release.await()
                inFlight.decrementAndGet()
            }
        }
        advanceUntilIdle()

        assertTrue("all three long polls must be in flight at once", allStarted.isCompleted)
        assertEquals("three groups polled concurrently", 3, peak.get())

        release.complete(Unit)
        advanceUntilIdle()
        job.join()
    }

    @Test
    fun `G-37 discrimination — the pre-fix serial loop never exceeds one in flight`() = runTest {
        val inFlight = AtomicInteger(0)
        val peak = AtomicInteger(0)

        runPollAllGroupsSerially(listOf("a", "b", "c")) {
            val now = inFlight.incrementAndGet()
            peak.updateAndGet { max -> if (now > max) now else max }
            inFlight.decrementAndGet()
        }

        assertEquals(
            "the pre-fix shape must be strictly serial — if this reports 3, the " +
                "concurrency test above proves nothing",
            1,
            peak.get(),
        )
    }

    @Test
    fun `G-37 — the fan-out is BOUNDED by the concurrency limit`() = runTest {
        val inFlight = AtomicInteger(0)
        val peak = AtomicInteger(0)
        val started = AtomicInteger(0)
        val release = CompletableDeferred<Unit>()

        val job = launch {
            runPollAllGroups(
                groupIds = listOf("a", "b", "c", "d", "e", "f", "g"),
                concurrency = 3,
            ) {
                started.incrementAndGet()
                val now = inFlight.incrementAndGet()
                peak.updateAndGet { max -> if (now > max) now else max }
                release.await()
                inFlight.decrementAndGet()
            }
        }
        advanceUntilIdle()

        assertEquals("never more than the bound in flight", 3, peak.get())
        assertEquals("the rest wait for a permit", 3, started.get())

        release.complete(Unit)
        advanceUntilIdle()
        job.join()
        assertEquals("every group is eventually polled", 7, started.get())
    }

    @Test
    fun `G-37 — one group's failure does not sink the rest`() = runTest {
        val polled = mutableListOf<String>()

        runPollAllGroups(
            groupIds = listOf("a", "boom", "c", "d"),
            concurrency = 3,
        ) { id ->
            if (id == "boom") throw IOException("DS unreachable for $id")
            polled.add(id)
        }

        assertEquals(
            "every other group must still be polled",
            listOf("a", "c", "d"),
            polled.sorted(),
        )
    }

    @Test
    fun `G-37 discrimination — an unguarded fan-out lets one failure cancel the rest`() =
        runTest {
            val polled = mutableListOf<String>()

            try {
                runPollAllGroupsUnguarded(listOf("a", "boom", "c", "d")) { id ->
                    if (id == "boom") throw IOException("DS unreachable for $id")
                    // suspend so the siblings are still in flight when "boom" throws
                    kotlinx.coroutines.yield()
                    polled.add(id)
                }
            } catch (_: IOException) {
                // the unguarded shape propagates it out of the whole fan-out
            }

            assertTrue(
                "the unguarded shape must LOSE groups to a sibling's failure — if it " +
                    "polls all three, the isolation test above proves nothing. Got: $polled",
                polled.size < 3,
            )
        }

    @Test
    fun `an empty group list is a no-op`() = runTest {
        var calls = 0
        runPollAllGroups(groupIds = emptyList(), concurrency = 3) { calls++ }
        assertEquals(0, calls)
    }

    @Test
    fun `a concurrency of zero is coerced to one rather than deadlocking`() = runTest {
        val polled = mutableListOf<String>()
        runPollAllGroups(groupIds = listOf("a", "b"), concurrency = 0) { polled.add(it) }
        assertEquals(listOf("a", "b"), polled)
    }

    // -----------------------------------------------------------------
    // The permanent pre-fix companions (the ADR-0044 C-61 pattern).
    // -----------------------------------------------------------------

    /**
     * The PRE-G-37-sprint-2 shape, kept permanently so the discrimination test
     * above keeps discriminating: ONE mutex for the whole repository, handed out
     * for every group id. Test-only — production uses [GroupLocks].
     */
    private class SingleRepositoryLock {
        private val theOneLock = Mutex()

        @Suppress("UNUSED_PARAMETER")
        fun forGroup(groupIdHex: String): Mutex = theOneLock
    }

    /**
     * A fan-out WITHOUT the supervisor scope and WITHOUT the per-leg catch — the
     * shape a naive "just make it concurrent" change produces. Under structured
     * concurrency the first child failure cancels its siblings, so groups that
     * were mid-poll are silently lost. Kept permanently so the isolation test
     * above keeps discriminating. Test-only — production uses [runPollAllGroups].
     */
    private suspend fun runPollAllGroupsUnguarded(
        groupIds: List<String>,
        pollOne: suspend (String) -> Unit,
    ) {
        kotlinx.coroutines.coroutineScope {
            for (groupIdHex in groupIds) {
                launch { pollOne(groupIdHex) }
            }
        }
    }

    /**
     * The PRE-G-37-sprint-2 chat-list poll: strictly serial, so N groups with a
     * 30 s long poll each cost `30 * N` seconds. Kept permanently so the
     * concurrency test above keeps discriminating. Test-only — production uses
     * [runPollAllGroups].
     */
    private suspend fun runPollAllGroupsSerially(
        groupIds: List<String>,
        pollOne: suspend (String) -> Unit,
    ) {
        for (groupIdHex in groupIds) {
            try {
                pollOne(groupIdHex)
            } catch (_: Throwable) {
                // best-effort, as the original loop was
            }
        }
    }
}
