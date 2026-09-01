// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.FfiRefusalReason
import uniffi.lakko_ffi_uniffi.OutgoingOwner

/**
 * ADR-0045 UX §7 — the automatic re-send of a request a full queue refused.
 * Android mirror of the web reference `apps/web/src/chat/membership-retry.test.ts`.
 *
 * ## Why the retry port is a STATED rule here and the real core elsewhere
 *
 * `membershipRefusalRetryMaySucceed` is a JNA call into
 * `liblakko_ffi_uniffi.so`, which the host JVM does not load — the same
 * host-vs-instrumented split `MembershipRefusalTest` /
 * `MembershipRefusalInstrumentedTest` already use, and the one this suite
 * inherits rather than re-deciding. So this suite drives the module through a
 * port that STATES the maintainer's rule, and pins that the module transmits
 * whatever the port says (including when the port is wrong). Agreement with
 * the CORE's own answer — the thing that would catch the rule itself drifting
 * — is proven in `MembershipRetryInstrumentedTest`, which runs where the `.so`
 * exists.
 *
 * Neither half is sufficient alone: the instrumented test cannot see a module
 * that hard-coded the partition (it would agree with the core on every input),
 * and this one cannot see the rule being restated wrongly.
 *
 * ## The concurrency test uses REAL platform threads
 *
 * A previous sprint (ADR-0044's serializing-lock arc) found that a confined
 * dispatcher masks a broken lock exactly as thoroughly as no concurrency does.
 * So the exactly-once probe starts real `Thread`s and releases them from one
 * latch, which is what makes it able to fail.
 */
class MembershipRetryTest {

    /**
     * The maintainer's rule, STATED here rather than read from the core:
     * retry when a request was refused because a bounded queue was full, never
     * when it was refused because of its own state.
     */
    private val real: (FfiRefusalReason) -> Boolean = {
        it == FfiRefusalReason.ROLE_CHANGE_PENDING || it == FfiRefusalReason.PENDING_SET_FULL
    }
    private val store = MembershipRetryStore()
    private val scope = "acct-1 group-1"

    private val alice = "QUxJQ0U="
    private val bob = "Qk9C"
    private val carol = "Q0FST0w="

    /**
     * Carol is NOT in the default roster, so an add of her is genuinely
     * outstanding — an add fixture naming somebody the group already contains
     * reads as `AlreadyApplied` instead, which is a different test.
     */
    private val addCarol =
        RetryableRequest.Add(subjectKeyB64 = carol, subjectAccountIdB64 = "Y2Fyb2wtYWNjdA==")
    private val removeBob = RetryableRequest.Remove(subjectKeyB64 = bob)
    private val leaveAlice = RetryableRequest.Leave(subjectKeyB64 = alice)
    private val makeBobAdmin = RetryableRequest.SetAdmin(subjectKeyB64 = bob, makeAdmin = true)
    private val transferToBob =
        RetryableRequest.TransferOwnership(subjectKeyB64 = bob, outgoing = OutgoingOwner.ADMIN)

    @Before
    fun reset() {
        store.clear(scope)
    }

    /** A group of Alice (owner, leaf 0) and Bob (leaf 1), nothing pending. */
    private fun snapshot(
        roster: List<RetryRosterMember>? =
            listOf(
                RetryRosterMember(0u, "QUxJQ0U="),
                RetryRosterMember(1u, "Qk9C"),
            ),
        pendingKeys: List<String> = emptyList(),
        pendingCount: Int = 0,
        roleTable: RetryRoleTable? = RetryRoleTable("QUxJQ0U=", emptyList()),
        membershipEnded: Boolean = false,
    ) = RetryGroupSnapshot(roster, pendingKeys, pendingCount, roleTable, membershipEnded)

    private fun armed(request: RetryableRequest, count: Int = 64) =
        ArmedRetry(request, FfiRefusalReason.PENDING_SET_FULL, count)

    // -----------------------------------------------------------------------
    // 1. Arming — the core decides, and only the core
    // -----------------------------------------------------------------------

    @Test
    fun `arms on each of the cores own RetryLater reasons`() {
        for (reason in listOf(FfiRefusalReason.PENDING_SET_FULL, FfiRefusalReason.ROLE_CHANGE_PENDING)) {
            assertTrue("$reason", real(reason))
            val out =
                armedRetryFor(
                    error = FfiException.MembershipPendingSetFull("core prose"),
                    request = addCarol,
                    pendingCountAtRefusal = 64,
                    explicitReason = reason,
                    retryMaySucceed = real,
                )
            assertNotNull("$reason", out)
            assertEquals(64, out!!.pendingCountAtRefusal)
        }
    }

    @Test
    fun `never arms on a DoNotRetry or NotPermitted reason`() {
        for (reason in FfiRefusalReason.entries.filterNot(real)) {
            assertNull(
                "$reason must never arm",
                armedRetryFor(
                    error = FfiException.Group("irrelevant"),
                    request = addCarol,
                    pendingCountAtRefusal = 64,
                    explicitReason = reason,
                    retryMaySucceed = real,
                ),
            )
        }
    }

    @Test
    fun `never arms on an error that carries no reason at all`() {
        assertNull(
            armedRetryFor(
                error = IllegalStateException("socket closed"),
                request = addCarol,
                pendingCountAtRefusal = 64,
                retryMaySucceed = real,
            ),
        )
    }

    @Test
    fun `finds the reason through the wrapper the repository throws`() {
        val outer =
            ChatError("addMemberToGroup failed", FfiException.MembershipPendingSetFull("prose"))
        assertNotNull(
            armedRetryFor(
                error = outer,
                request = addCarol,
                pendingCountAtRefusal = 64,
                retryMaySucceed = real,
            ),
        )
    }

    /**
     * The probe: prove the module ASKS rather than keeping its own list. A
     * hard-coded module agrees with the real core on every input and passes
     * every test above.
     */
    @Test
    fun `follows the port even when the port is wrong, in both directions`() {
        val invertedTrue: (FfiRefusalReason) -> Boolean = {
            it == FfiRefusalReason.ADD_NOT_OWNER_OR_ADMIN
        }
        assertNotNull(
            "a port that says a permission refusal is retryable must be believed",
            armedRetryFor(
                error = FfiException.MembershipAddNotOwnerOrAdmin("x"),
                request = addCarol,
                pendingCountAtRefusal = 64,
                retryMaySucceed = invertedTrue,
            ),
        )

        val invertedFalse: (FfiRefusalReason) -> Boolean = { false }
        assertNull(
            "a port that says pendingSetFull is NOT retryable must be believed",
            armedRetryFor(
                error = FfiException.MembershipPendingSetFull("x"),
                request = addCarol,
                pendingCountAtRefusal = 64,
                retryMaySucceed = invertedFalse,
            ),
        )
    }

    // -----------------------------------------------------------------------
    // 2. The trigger — the queue drained, and never a clock
    // -----------------------------------------------------------------------

    @Test
    fun `waits while the queue has not drained`() {
        assertEquals(RetryVerdict.Wait, retryVerdict(armed(addCarol), snapshot(pendingCount = 64)))
    }

    @Test
    fun `waits when the queue has grown`() {
        assertEquals(RetryVerdict.Wait, retryVerdict(armed(addCarol), snapshot(pendingCount = 70)))
    }

    @Test
    fun `fires once the queue is strictly smaller than at the refusal`() {
        assertEquals(RetryVerdict.Fire, retryVerdict(armed(addCarol), snapshot(pendingCount = 63)))
        assertEquals(RetryVerdict.Fire, retryVerdict(armed(addCarol), snapshot(pendingCount = 0)))
    }

    @Test
    fun `cannot fire from the passage of time alone`() {
        val a = armed(addCarol)
        val view = snapshot(pendingCount = 64)
        repeat(5) { assertEquals(RetryVerdict.Wait, retryVerdict(a, view)) }
    }

    @Test
    fun `claims nothing on a torn roster read`() {
        assertEquals(
            RetryVerdict.Wait,
            retryVerdict(armed(addCarol), snapshot(roster = null, pendingCount = 0)),
        )
    }

    // -----------------------------------------------------------------------
    // 3. Stopping conditions — every one a state, none a count
    // -----------------------------------------------------------------------

    @Test
    fun `drops an add once the person is in the roster`() {
        val view =
            snapshot(
                roster =
                    listOf(
                        RetryRosterMember(0u, alice),
                        RetryRosterMember(1u, bob),
                        RetryRosterMember(2u, carol),
                    ),
                pendingCount = 0,
            )
        assertEquals(RetryVerdict.Drop(RetryDropReason.ALREADY_APPLIED), retryVerdict(armed(addCarol), view))
    }

    @Test
    fun `drops a remove once the person has left the roster`() {
        val view = snapshot(roster = listOf(RetryRosterMember(0u, alice)), pendingCount = 0)
        assertEquals(RetryVerdict.Drop(RetryDropReason.ALREADY_APPLIED), retryVerdict(armed(removeBob), view))
    }

    @Test
    fun `drops a leave once our own leaf has gone`() {
        val view = snapshot(roster = listOf(RetryRosterMember(1u, bob)), pendingCount = 0)
        assertEquals(RetryVerdict.Drop(RetryDropReason.ALREADY_APPLIED), retryVerdict(armed(leaveAlice), view))
    }

    @Test
    fun `drops a promotion once the live table says it`() {
        val view = snapshot(pendingCount = 0, roleTable = RetryRoleTable(alice, listOf(bob)))
        assertEquals(
            RetryVerdict.Drop(RetryDropReason.ALREADY_APPLIED),
            retryVerdict(armed(makeBobAdmin), view),
        )
    }

    @Test
    fun `drops a demotion once the member is no longer an admin`() {
        val demote = RetryableRequest.SetAdmin(subjectKeyB64 = bob, makeAdmin = false)
        assertEquals(
            RetryVerdict.Drop(RetryDropReason.ALREADY_APPLIED),
            retryVerdict(armed(demote), snapshot(pendingCount = 0, roleTable = RetryRoleTable(alice, emptyList()))),
        )
        assertEquals(
            RetryVerdict.Wait,
            retryVerdict(
                armed(demote),
                snapshot(pendingCount = 64, roleTable = RetryRoleTable(alice, listOf(bob))),
            ),
        )
    }

    @Test
    fun `drops a transfer once the successor holds the group`() {
        val view = snapshot(pendingCount = 0, roleTable = RetryRoleTable(bob, listOf(alice)))
        assertEquals(
            RetryVerdict.Drop(RetryDropReason.ALREADY_APPLIED),
            retryVerdict(armed(transferToBob), view),
        )
    }

    @Test
    fun `keeps waiting on a transfer the live table has not yet made`() {
        assertEquals(RetryVerdict.Wait, retryVerdict(armed(transferToBob), snapshot(pendingCount = 64)))
    }

    @Test
    fun `drops once the request is itself waiting in the pending set`() {
        assertEquals(
            RetryVerdict.Drop(RetryDropReason.ALREADY_WAITING),
            retryVerdict(armed(addCarol), snapshot(pendingKeys = listOf("add:$carol"), pendingCount = 1)),
        )
    }

    @Test
    fun `resolves a pending remove through the roster rather than a stored leaf`() {
        assertEquals(
            RetryVerdict.Drop(RetryDropReason.ALREADY_WAITING),
            retryVerdict(armed(removeBob), snapshot(pendingKeys = listOf("remove:1"), pendingCount = 1)),
        )
        // A pending remove of somebody ELSE's leaf is not ours.
        assertEquals(
            RetryVerdict.Fire,
            retryVerdict(armed(removeBob), snapshot(pendingKeys = listOf("remove:7"), pendingCount = 1)),
        )
    }

    @Test
    fun `drops a role change already waiting, whatever it says`() {
        assertEquals(
            RetryVerdict.Drop(RetryDropReason.ALREADY_WAITING),
            retryVerdict(armed(makeBobAdmin), snapshot(pendingKeys = listOf("role-change"), pendingCount = 1)),
        )
    }

    @Test
    fun `drops everything once our own membership has ended`() {
        assertEquals(
            RetryVerdict.Drop(RetryDropReason.MEMBERSHIP_ENDED),
            retryVerdict(armed(addCarol), snapshot(pendingCount = 0, membershipEnded = true)),
        )
    }

    @Test
    fun `drops a role change in a group that has lost its role table`() {
        assertEquals(
            RetryVerdict.Drop(RetryDropReason.NO_ROLE_TABLE),
            retryVerdict(armed(makeBobAdmin), snapshot(pendingCount = 0, roleTable = null)),
        )
    }

    // -----------------------------------------------------------------------
    // 4. Exactly-once
    // -----------------------------------------------------------------------

    @Test
    fun `hands the record to exactly one of two claims`() {
        store.arm(scope, armed(addCarol))
        assertNotNull(store.claim(scope, addCarol.key))
        assertNull(store.claim(scope, addCarol.key))
    }

    /**
     * REAL platform threads, released from one latch.
     *
     * `Dispatchers.IO` would have been the convenient choice and is the one a
     * previous sprint proved masks a broken lock: a confined or pooled
     * dispatcher can serialize the claims by accident and pass whatever the
     * store does.
     */
    @Test
    fun `exactly one of many concurrent threads wins the claim`() {
        repeat(200) {
            store.arm(scope, armed(addCarol))
            val winners = ConcurrentLinkedQueue<ArmedRetry>()
            val start = CountDownLatch(1)
            val done = CountDownLatch(8)
            repeat(8) {
                Thread {
                        start.await()
                        store.claim(scope, addCarol.key)?.let(winners::add)
                        done.countDown()
                    }
                    .start()
            }
            start.countDown()
            assertTrue("threads must finish", done.await(10, TimeUnit.SECONDS))
            assertEquals("exactly one thread may own the record", 1, winners.size)
        }
    }

    @Test
    fun `holds at most one record per request`() {
        store.arm(scope, armed(addCarol, 64))
        store.arm(scope, armed(addCarol, 12))
        assertEquals(1, store.armed(scope).size)
        assertEquals(12, store.armed(scope).first().pendingCountAtRefusal)
    }

    @Test
    fun `a manual tap disarms the record`() {
        store.arm(scope, armed(addCarol))
        assertNotNull(store.claim(scope, addCarol.key))
        assertNull(store.claim(scope, addCarol.key))
        assertTrue(store.armed(scope).isEmpty())
    }

    @Test
    fun `keeps different requests apart`() {
        store.arm(scope, armed(addCarol))
        store.arm(scope, armed(removeBob))
        assertEquals(2, store.armed(scope).size)
        store.claim(scope, addCarol.key)
        assertEquals(1, store.armed(scope).size)
    }

    @Test
    fun `scopes records per group`() {
        store.arm(scope, armed(addCarol))
        assertTrue(store.armed("acct-1 group-2").isEmpty())
        assertEquals(1, store.armed(scope).size)
    }

    @Test
    fun `shares one armed slot between the two role-change shapes`() {
        assertEquals(makeBobAdmin.key, transferToBob.key)
        store.arm(scope, armed(makeBobAdmin))
        store.arm(scope, armed(transferToBob))
        assertEquals(1, store.armed(scope).size)
        assertTrue(store.armed(scope).first().request is RetryableRequest.TransferOwnership)
    }

    @Test
    fun `keys a remove on the signature key, never on a reusable leaf`() {
        assertTrue(removeBob.key.contains(bob))
        assertTrue(removeBob.key != "remove:1")
    }
}
