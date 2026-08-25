// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * ADR-0045 / ADR-0027 (G-3) — host-side tests for [runApproveExternalJoin], the
 * ordering that lets an external-join `Approve` survive a non-empty pending
 * proposal set.
 *
 * ## What was broken
 *
 * `Group::approve_external_join` opens with `refuse_if_proposals_pending`, a bare
 * count of ANY pending proposal of ANY kind. Under ADR-0045 a non-empty pending
 * set is the ORDINARY state between any member's request and the next committer
 * sweep, so an admin tapping `Approve` hit a hard failure whenever anything at
 * all was waiting — and what reached the screen was the core's own diagnostic
 * naming `commit_pending_proposals` / `stage_pending_proposals`, two Rust entry
 * points no user can call.
 *
 * ## Why these are host tests over a seam rather than a live-group test
 *
 * `ChatRepository` needs an Android `Context`, and the FFI `Group` needs the JNI
 * native library, which a JVM unit test cannot load — the same constraint that
 * put [runSplitPoll] and [runCrashSafeCommit] behind injected steps. So the
 * production function is driven here with the real ordering and fake steps. The
 * live-MLS half — that the core really does refuse, and that a real sweep really
 * does clear the set — is covered by the iOS `ExternalJoinApprovalTests`, whose
 * simulator test target CAN link the FFI; the two platforms run the same
 * ordering, so pinning the core's behaviour once is enough.
 *
 * Properties pinned here:
 *
 *  1. The drain runs, and an acting committer that clears the pending set
 *     approves through it. **The main fix.**
 *  2. Discrimination for (1): [runApproveExternalJoinWithoutDrain] — a permanent
 *     companion holding the PRE-FIX shape — is asserted to FAIL the property the
 *     fixed version passes, so a refactor that quietly dropped the drain cannot
 *     leave this suite green.
 *  3. When the drain cannot clear the set (this device is not the acting
 *     committer), the refusal is [MembershipChangesPendingException] carrying
 *     human copy — never the core's vocabulary — and the commit is never built.
 *  4. The drain holds NEITHER lock. Run for real against a timeout, because the
 *     failure mode of getting this wrong is a silent hang, not an exception.
 *  5. Discrimination for (4): [runApproveExternalJoinDrainingInsideTheLocks] —
 *     the inverted shape — is asserted to hang.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ApproveExternalJoinTest {

    /**
     * A pending set with a drain that models one device's committer standing.
     *
     * `clearsOnDrain = true` is the acting committer (its sweep commits the
     * pending proposals); `false` is a candidate standing down behind a
     * better-placed one's beacon, whose sweep is a no-op.
     */
    private class FakeGroup(pending: Int, private val clearsOnDrain: Boolean) {
        var pending: Int = pending
            private set
        var drains: Int = 0
            private set
        var prepared: Int = 0
            private set
        var published: Int = 0
            private set

        fun drain() {
            drains += 1
            if (clearsOnDrain) pending = 0
        }

        fun prepare(): suspend () -> Long {
            prepared += 1
            return {
                published += 1
                42L
            }
        }
    }

    // -----------------------------------------------------------------
    // 1. the fix: an acting committer approves through a pending set
    // -----------------------------------------------------------------

    @Test
    fun `ADR-0045 — the drain clears the pending set so an acting committer can approve`() =
        runBlocking {
            val group = FakeGroup(pending = 1, clearsOnDrain = true)

            val id = runApproveExternalJoin(
                drain = { group.drain() },
                lock = Mutex(),
                providerLock = Mutex(),
                pendingProposalCount = { group.pending },
                prepare = { group.prepare() },
            )

            assertEquals("the approve must reach the DS", 42L, id)
            assertEquals("the drain must have run exactly once", 1, group.drains)
            assertEquals("the commit must have been built", 1, group.prepared)
            assertEquals("the commit must have been published", 1, group.published)
        }

    @Test
    fun `ADR-0045 discrimination — without the drain the same approve is refused`() = runBlocking {
        val group = FakeGroup(pending = 1, clearsOnDrain = true)

        try {
            runApproveExternalJoinWithoutDrain(
                lock = Mutex(),
                providerLock = Mutex(),
                pendingProposalCount = { group.pending },
                prepare = { group.prepare() },
            )
            fail(
                "the PRE-FIX shape must still fail the property the fixed one passes — if this " +
                    "ever succeeds, the test above has stopped discriminating",
            )
        } catch (e: MembershipChangesPendingException) {
            assertEquals("the pending set must be untouched", 1, group.pending)
            assertEquals("no commit may be built", 0, group.prepared)
        }
        Unit
    }

    // -----------------------------------------------------------------
    // 3. the surviving refusal reads as human copy
    // -----------------------------------------------------------------

    @Test
    fun `ADR-0045 — a drain that cannot clear the set refuses in human copy`() = runBlocking {
        val group = FakeGroup(pending = 2, clearsOnDrain = false)

        try {
            runApproveExternalJoin(
                drain = { group.drain() },
                lock = Mutex(),
                providerLock = Mutex(),
                pendingProposalCount = { group.pending },
                prepare = { group.prepare() },
            )
            fail("the approve must not proceed while this device cannot commit")
        } catch (e: MembershipChangesPendingException) {
            assertEquals(
                "the message IS the user-facing copy — the ViewModel renders `e.message`",
                MEMBERSHIP_CHANGES_PENDING_COPY,
                e.message,
            )
            // The point of the whole case: none of the core's vocabulary reaches
            // the screen.
            for (leak in listOf(
                "commit_pending_proposals",
                "stage_pending_proposals",
                "membership proposal(s) are pending",
                "ADR-0045",
            )) {
                assertFalse(
                    "user-facing copy must not contain the developer string '$leak'",
                    e.message!!.contains(leak),
                )
            }
            assertEquals("the drain must still have been attempted", 1, group.drains)
            assertEquals("no commit may be built on the refusal path", 0, group.prepared)
        }
        Unit
    }

    @Test
    fun `ADR-0045 — the copy states inaction without naming a person or a deadline`() {
        val copy = MEMBERSHIP_CHANGES_PENDING_COPY
        // `docs/ux/proposal-based-membership.md` section 5.1: the liveness
        // sentence names no person, claims no knowledge of who is online, and
        // implies no deadline (ADR-0045 decision 2 gives a pending request none).
        for (forbidden in listOf("online", "soon", "shortly", "minute", "hour", "try again later")) {
            assertFalse(
                "copy must not imply a deadline or a person's availability: '$forbidden'",
                copy.lowercase().contains(forbidden),
            )
        }
        assertTrue(
            "copy must reuse the spec's own phrasing for who applies changes",
            copy.contains("applied by the group owner or an admin"),
        )
    }

    // -----------------------------------------------------------------
    // 4. the drain must hold NEITHER lock
    // -----------------------------------------------------------------

    /**
     * A kotlinx [Mutex] is not reentrant, so the sweep the real `drain` runs —
     * which takes both locks itself — deadlocks if it is called from inside
     * either. The failure mode is a HANG, so this is run for real, on real
     * dispatchers, against a timeout: an assertion about lock ordering that
     * cannot hang is not an assertion about lock ordering.
     */
    @Test
    fun `ADR-0045 — the drain runs outside both locks, so a reentrant sweep cannot deadlock`() =
        runBlocking {
            withContext(Dispatchers.IO) {
                val lock = Mutex()
                val providerLock = Mutex()
                val group = FakeGroup(pending = 1, clearsOnDrain = true)

                val id = withTimeout(5_000) {
                    runApproveExternalJoin(
                        // Exactly what `sweepIfCommitter` does: take the per-group
                        // lock, then the provider lock.
                        drain = {
                            lock.withLock { providerLock.withLock { group.drain() } }
                        },
                        lock = lock,
                        providerLock = providerLock,
                        pendingProposalCount = { group.pending },
                        prepare = { group.prepare() },
                    )
                }

                assertEquals(42L, id)
                assertEquals(1, group.drains)
            }
        }

    @Test
    fun `ADR-0045 discrimination — draining inside the locks hangs`() = runBlocking {
        withContext(Dispatchers.IO) {
            val lock = Mutex()
            val providerLock = Mutex()
            val group = FakeGroup(pending = 1, clearsOnDrain = true)

            try {
                withTimeout(1_000) {
                    runApproveExternalJoinDrainingInsideTheLocks(
                        drain = {
                            lock.withLock { providerLock.withLock { group.drain() } }
                        },
                        lock = lock,
                        providerLock = providerLock,
                        pendingProposalCount = { group.pending },
                        prepare = { group.prepare() },
                    )
                }
                fail(
                    "the inverted shape must deadlock — if it completes, a kotlinx Mutex has " +
                        "become reentrant and the reason the drain sits outside the locks is gone",
                )
            } catch (e: TimeoutCancellationException) {
                assertEquals("the drain never got past the outer lock", 0, group.drains)
            }
            Unit
        }
    }

    // -----------------------------------------------------------------
    // permanent inverted companions — the PRE-FIX / wrong-order shapes
    // -----------------------------------------------------------------

    /** The shape before this fix: straight to the locks, no drain. */
    private suspend fun <T> runApproveExternalJoinWithoutDrain(
        lock: Mutex,
        providerLock: Mutex,
        pendingProposalCount: () -> Int,
        prepare: () -> (suspend () -> T),
    ): T = lock.withLock {
        val publishStep = providerLock.withLock {
            if (pendingProposalCount() > 0) throw MembershipChangesPendingException()
            prepare()
        }
        publishStep()
    }

    /** The tempting-but-fatal shape: drain inside the critical section. */
    private suspend fun <T> runApproveExternalJoinDrainingInsideTheLocks(
        drain: suspend () -> Unit,
        lock: Mutex,
        providerLock: Mutex,
        pendingProposalCount: () -> Int,
        prepare: () -> (suspend () -> T),
    ): T = lock.withLock {
        val publishStep = providerLock.withLock {
            prepare()
        }
        drain()
        if (pendingProposalCount() > 0) throw MembershipChangesPendingException()
        publishStep()
    }
}
