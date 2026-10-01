// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.FfiRefusalReason
import uniffi.lakko_ffi_uniffi.OutgoingOwner

/**
 * ADR-0050 section 2.5a point 4 — re-requesting an own membership request a
 * sweep dropped. Every rule lives in `MembershipRetry.kt` / `PendingRequests.kt`
 * so it runs here without an FFI group, a `Context` or a network.
 */
class SupersededRequestTest {

    private val account = "acct"
    private val group = "grp"

    // Signature keys, base64 as the client carries them.
    private val owner = "b3duZXI="
    private val admin = "YWRtaW4="
    private val alice = "YWxpY2U="
    private val bob = "Ym9i"
    private val carol = "Y2Fyb2w="
    private val me = "bWU="
    private val newcomer = "bmV3Y29tZXI="
    private val aliceAccount = "YWxpY2UtYWNjb3VudA=="

    private class MemStore : PendingRequestStore {
        val rows = mutableMapOf<String, Map<String, RequestObservation>>()
        override fun load(accountIdB64: String, groupIdHex: String) =
            rows["$accountIdB64/$groupIdHex"] ?: emptyMap()

        override fun save(
            accountIdB64: String,
            groupIdHex: String,
            observations: Map<String, RequestObservation>,
        ) {
            rows["$accountIdB64/$groupIdHex"] = observations
        }

        override fun clearForAccount(accountIdB64: String) {
            rows.keys.filter { it.startsWith("$accountIdB64/") }.forEach { rows.remove(it) }
        }
    }

    private class MemUnapplied : UnappliedRequestStore {
        val rows = mutableMapOf<String, Map<String, UnappliedRow>>()
        override fun load(accountIdB64: String, groupIdHex: String) =
            rows["$accountIdB64/$groupIdHex"] ?: emptyMap()

        override fun save(accountIdB64: String, groupIdHex: String, rows: Map<String, UnappliedRow>) {
            this.rows["$accountIdB64/$groupIdHex"] = rows
        }

        override fun clearForAccount(accountIdB64: String) = Unit
        override fun clearForGroup(accountIdB64: String, groupIdHex: String) = Unit
    }

    private fun roster(vararg members: Pair<UInt, String>): List<RetryRosterMember> =
        members.map { RetryRosterMember(it.first, it.second) }

    private fun snapshot(
        roster: List<RetryRosterMember>? = roster(0u to owner, 1u to admin, 2u to me, 3u to alice, 4u to bob),
        pending: List<String> = emptyList(),
        admins: List<String> = listOf(admin),
        ownerKey: String = owner,
        table: Boolean = true,
        ended: Boolean = false,
        retained: List<String>? = emptyList(),
    ) = RetryGroupSnapshot(
        roster = roster,
        pendingKeys = pending,
        pendingCount = pending.size,
        roleTable = if (table) RetryRoleTable(ownerKey, admins) else null,
        membershipEnded = ended,
        retainedJoinKeys = retained,
    )

    private val landed = SupersededVerdict.Clear(SupersededClearReason.LANDED)

    // -----------------------------------------------------------------------
    // Detection, per kind
    // -----------------------------------------------------------------------

    @Test
    fun `an add is landed, waiting, or superseded`() {
        val intent = RetryableRequest.Add(carol, aliceAccount)
        assertEquals(
            landed,
            supersededVerdict(intent, snapshot(roster = roster(0u to owner, 5u to carol))),
        )
        assertEquals(SupersededVerdict.Keep, supersededVerdict(intent, snapshot(pending = listOf("add:$carol"))))
        assertEquals(SupersededVerdict.ReRequest, supersededVerdict(intent, snapshot()))
    }

    /**
     * The reason a removal is keyed on a SIGNATURE KEY: OpenMLS hands a vacated
     * leaf to the next Add. Bob was at leaf 4 when this device asked to remove
     * him; a newcomer now holds leaf 4 and somebody else's removal of THAT leaf
     * is pending, while Bob sits at leaf 6. A leaf-keyed record would read the
     * pending `remove:4` as "ours is still waiting" and never re-request.
     */
    @Test
    fun `a removal is matched by signature key across a reused leaf`() {
        val intent = RetryableRequest.Remove(bob)
        val reused = snapshot(
            roster = roster(0u to owner, 4u to newcomer, 6u to bob),
            pending = listOf("remove:4"),
        )
        assertEquals(SupersededVerdict.ReRequest, supersededVerdict(intent, reused))
        // Bob is gone and his old leaf names somebody else: landed, not waiting.
        val gone = snapshot(roster = roster(0u to owner, 4u to newcomer), pending = listOf("remove:4"))
        assertEquals(landed, supersededVerdict(intent, gone))
        // And our removal of Bob, at his CURRENT leaf, is still waiting.
        val waiting = snapshot(roster = roster(0u to owner, 6u to bob), pending = listOf("remove:6"))
        assertEquals(SupersededVerdict.Keep, supersededVerdict(intent, waiting))
    }

    @Test
    fun `a role change is judged by its delta, never by a whole table`() {
        val promote = RetryableRequest.SetAdmin(alice, makeAdmin = true)
        assertEquals(landed, supersededVerdict(promote, snapshot(admins = listOf(admin, alice))))
        assertEquals(SupersededVerdict.ReRequest, supersededVerdict(promote, snapshot()))
        assertEquals(SupersededVerdict.Keep, supersededVerdict(promote, snapshot(pending = listOf("role-change"))))
        // Another admin was demoted meanwhile; ours (promote Alice) still applies
        // as a delta, so it is re-requested rather than read as "the table moved".
        assertEquals(SupersededVerdict.ReRequest, supersededVerdict(promote, snapshot(admins = emptyList())))

        val demote = RetryableRequest.SetAdmin(admin, makeAdmin = false)
        assertEquals(landed, supersededVerdict(demote, snapshot(admins = emptyList())))

        val transfer = RetryableRequest.TransferOwnership(alice, OutgoingOwner.ADMIN)
        assertEquals(landed, supersededVerdict(transfer, snapshot(ownerKey = alice)))
        assertEquals(SupersededVerdict.ReRequest, supersededVerdict(transfer, snapshot()))
    }

    @Test
    fun `a role change about somebody who left is forgotten, not listed`() {
        val promote = RetryableRequest.SetAdmin(carol, makeAdmin = true)
        assertEquals(
            SupersededVerdict.Clear(SupersededClearReason.SUBJECT_LEFT),
            supersededVerdict(promote, snapshot()),
        )
        val transfer = RetryableRequest.TransferOwnership(carol, OutgoingOwner.MEMBER)
        assertEquals(
            SupersededVerdict.Clear(SupersededClearReason.SUBJECT_LEFT),
            supersededVerdict(transfer, snapshot()),
        )
    }

    @Test
    fun `a leave is landed when the membership ended, waiting while pending, else re-requested`() {
        val leave = RetryableRequest.Leave(me)
        assertEquals(
            SupersededVerdict.Clear(SupersededClearReason.MEMBERSHIP_ENDED),
            supersededVerdict(leave, snapshot(ended = true)),
        )
        assertEquals(SupersededVerdict.Keep, supersededVerdict(leave, snapshot(pending = listOf("remove:2"))))
        assertEquals(SupersededVerdict.ReRequest, supersededVerdict(leave, snapshot()))
    }

    @Test
    fun `an approved join is re-approved only while the joiner is back in the quarantine`() {
        val approve = RetryableRequest.ApproveJoin(carol)
        assertEquals(SupersededVerdict.ReRequest, supersededVerdict(approve, snapshot(retained = listOf(carol))))
        // The approve consumed the retained KeyPackage: only the joiner can ask
        // again, so it is terminal here and listed.
        assertEquals(SupersededVerdict.NotApplied, supersededVerdict(approve, snapshot(retained = emptyList())))
        assertEquals(SupersededVerdict.Keep, supersededVerdict(approve, snapshot(retained = null)))
        assertEquals(SupersededVerdict.Keep, supersededVerdict(approve, snapshot(pending = listOf("add:$carol"))))
        assertEquals(
            landed,
            supersededVerdict(approve, snapshot(roster = roster(0u to owner, 9u to carol))),
        )
    }

    @Test
    fun `a torn read claims nothing and a lost role table stops a role change`() {
        for (intent in listOf(
            RetryableRequest.Add(carol, aliceAccount),
            RetryableRequest.Remove(bob),
            RetryableRequest.SetAdmin(alice, true),
        )) {
            assertEquals(SupersededVerdict.Keep, supersededVerdict(intent, snapshot(roster = null)))
        }
        assertEquals(
            SupersededVerdict.NotApplied,
            supersededVerdict(RetryableRequest.SetAdmin(alice, true), snapshot(table = false)),
        )
    }

    // -----------------------------------------------------------------------
    // Re-derivation
    // -----------------------------------------------------------------------

    /**
     * The point of re-deriving a role change: re-sending the table the user saw
     * would revert Bob's promotion, which landed after our request was dropped.
     */
    @Test
    fun `a role-change re-request does not revert a change that landed meanwhile`() {
        val a = byteArrayOf(1)
        val b = byteArrayOf(2) // promoted by somebody else after our request
        val x = byteArrayOf(3)
        val promoted = adminsWithDelta(listOf(a, b), x, makeAdmin = true)
        assertEquals(listOf(listOf(1.toByte()), listOf(2.toByte()), listOf(3.toByte())), promoted.map { it.toList() })
        val demoted = adminsWithDelta(listOf(a, b, x), a, makeAdmin = false)
        assertEquals(listOf(listOf(2.toByte()), listOf(3.toByte())), demoted.map { it.toList() })
        // Idempotent: promoting somebody already an admin adds no duplicate.
        assertEquals(2, adminsWithDelta(listOf(a, x), x, makeAdmin = true).size)
    }

    // -----------------------------------------------------------------------
    // Stop conditions
    // -----------------------------------------------------------------------

    @Test
    fun `a refused re-request stops unless the core says waiting helps`() {
        val retryLater = { r: FfiRefusalReason -> r == FfiRefusalReason.PENDING_SET_FULL }
        assertEquals(
            ReRequestFailure.NOT_APPLIED,
            reRequestFailure(ChatError("x", FfiException.MembershipAddNotOwnerOrAdmin("p")), retryLater),
        )
        assertEquals(
            ReRequestFailure.KEEP,
            reRequestFailure(ChatError("x", FfiException.MembershipPendingSetFull("p")), retryLater),
        )
        assertEquals(ReRequestFailure.KEEP, reRequestFailure(java.io.IOException("socket"), retryLater))
        // It ASKS the port: the same reason reads the other way when the port does.
        assertEquals(
            ReRequestFailure.KEEP,
            reRequestFailure(FfiException.MembershipAddNotOwnerOrAdmin("p")) { true },
        )
    }

    @Test
    fun `the pass re-requests, forgets, lists and keeps by state`() = runBlocking {
        val store = MemStore()
        val notApplied = MemUnapplied()
        val intents = listOf(
            RetryableRequest.Add(carol, aliceAccount), // superseded -> re-request ok
            RetryableRequest.Remove(newcomer), // landed (not in roster)
            RetryableRequest.SetAdmin(alice, true), // re-request refused NotPermitted
            RetryableRequest.Add(alice, aliceAccount), // landed (in the roster)
            RetryableRequest.Leave(me), // re-request refused RetryLater -> kept
        )
        for (i in intents) recordOwnRequestIntent(store, account, group, i, atMs = 1L)
        val sent = mutableListOf<RetryableRequest>()
        val result = runSupersededPass(
            snapshot = snapshot(),
            store = store,
            notApplied = notApplied,
            accountIdB64 = account,
            groupIdHex = group,
            reRequest = { intent ->
                sent += intent
                when (intent) {
                    is RetryableRequest.SetAdmin -> throw ChatError("x", FfiException.MembershipNotOwner("p"))
                    is RetryableRequest.Leave -> throw ChatError("x", FfiException.MembershipPendingSetFull("p"))
                    else -> true
                }
            },
            retryMaySucceed = { it == FfiRefusalReason.PENDING_SET_FULL },
        )
        assertEquals(SupersededPassResult(reRequested = 1, cleared = 2, notApplied = 1, kept = 1), result)
        assertEquals(
            listOf(
                RetryableRequest.Add(carol, aliceAccount),
                RetryableRequest.SetAdmin(alice, true),
                RetryableRequest.Leave(me),
            ),
            sent,
        )
        // Landed and stopped intents are forgotten; the re-sent one and the
        // RetryLater one stay for the next commit.
        assertEquals(
            setOf(RetryableRequest.Add(carol, aliceAccount), RetryableRequest.Leave(me)),
            loadOwnRequestIntents(store, account, group).toSet(),
        )
        // Only the stopped one is shown; a re-request in flight is not.
        assertEquals(
            listOf(UnappliedRequest(alice, UnappliedKind.MAKE_ADMIN)),
            loadUnapplied(notApplied, account, group),
        )
    }

    @Test
    fun `a re-request whose subject is gone is forgotten silently`() = runBlocking {
        val store = MemStore()
        val notApplied = MemUnapplied()
        recordOwnRequestIntent(store, account, group, RetryableRequest.Add(carol, aliceAccount), 1L)
        val result = runSupersededPass(
            snapshot(), store, notApplied, account, group,
            reRequest = { false },
            retryMaySucceed = { false },
        )
        assertEquals(1, result.cleared)
        assertTrue(loadOwnRequestIntents(store, account, group).isEmpty())
        assertTrue(loadUnapplied(notApplied, account, group).isEmpty())
    }

    @Test
    fun `nothing is sent for a request that is still pending or landed`() = runBlocking {
        val store = MemStore()
        recordOwnRequestIntent(store, account, group, RetryableRequest.Add(carol, aliceAccount), 1L)
        recordOwnRequestIntent(store, account, group, RetryableRequest.Remove(newcomer), 1L)
        val result = runSupersededPass(
            snapshot(pending = listOf("add:$carol")), store, MemUnapplied(), account, group,
            reRequest = { error("must not be called") },
            retryMaySucceed = { false },
        )
        assertEquals(0, result.reRequested)
        assertEquals(1, result.kept)
    }

    // -----------------------------------------------------------------------
    // The trigger: an observed merged commit, never a timer
    // -----------------------------------------------------------------------

    @Test
    fun `the check runs only when the epoch moved`() {
        // A fresh session with nothing to compare against does not run.
        assertFalse(supersededCheckDue(baseline = null, epochBefore = null, epochNow = 7uL))
        // A poll that applied no commit does not run, however often it polls.
        repeat(5) { assertFalse(supersededCheckDue(baseline = 7uL, epochBefore = 7uL, epochNow = 7uL)) }
        assertFalse(supersededCheckDue(baseline = null, epochBefore = 7uL, epochNow = 7uL))
        // A merged commit does.
        assertTrue(supersededCheckDue(baseline = 7uL, epochBefore = 7uL, epochNow = 8uL))
        assertTrue(supersededCheckDue(baseline = null, epochBefore = 7uL, epochNow = 8uL))
        // The baseline wins over the batch's own start: a sweep merged between
        // two polls is still a commit this pass has not looked at.
        assertTrue(supersededCheckDue(baseline = 6uL, epochBefore = 7uL, epochNow = 7uL))
    }

    // -----------------------------------------------------------------------
    // The intent record
    // -----------------------------------------------------------------------

    @Test
    fun `every intent survives the row codec`() {
        val intents = listOf(
            RetryableRequest.Add(carol, aliceAccount),
            RetryableRequest.Remove(bob),
            RetryableRequest.Leave(me),
            RetryableRequest.SetAdmin(alice, true),
            RetryableRequest.SetAdmin(alice, false),
            RetryableRequest.TransferOwnership(alice, OutgoingOwner.ADMIN),
            RetryableRequest.TransferOwnership(alice, OutgoingOwner.MEMBER),
            RetryableRequest.ApproveJoin(carol),
        )
        for (i in intents) assertEquals(i, decodeRequestIntent(encodeRequestIntent(i)))
        val rows = intents.associate { intentObservationKey(it) + it.hashCode() to RequestObservation(1L, true, intent = it) }
        assertEquals(rows, SharedPrefsPendingRequestStore.decodeRow(SharedPrefsPendingRequestStore.encodeRow(rows)))
        for (bad in listOf("", "add", "add,k", "set-admin,k,2", "transfer,k,KING", "nope,k")) {
            assertNull(bad, decodeRequestIntent(bad))
        }
    }

    @Test
    fun `an intent outlives the prune that drops its proposal`() {
        val store = MemStore()
        recordPendingRequestObservation(store, account, group, "remove:4", 100, own = true)
        recordOwnRequestIntent(store, account, group, RetryableRequest.Remove(bob), 100)
        prunePendingRequestObservations(store, account, group, liveKeys = emptyList())
        assertEquals(setOf("intent:remove:$bob"), store.load(account, group).keys)
        // And the panel fold never renders it.
        assertTrue(foldPendingRequests(emptyList(), store.load(account, group), nowMs = 0L).requests.isEmpty())
    }

    @Test
    fun `the newest intent for a key wins and forgetting spares a newer one`() {
        val store = MemStore()
        recordOwnRequestIntent(store, account, group, RetryableRequest.SetAdmin(alice, true), 1L)
        recordOwnRequestIntent(store, account, group, RetryableRequest.SetAdmin(bob, false), 2L)
        // One role-change slot: the demote replaced the promote.
        assertEquals(
            listOf<RetryableRequest>(RetryableRequest.SetAdmin(bob, false)),
            loadOwnRequestIntents(store, account, group),
        )
        // A pass that read the OLD intent must not delete the new one.
        forgetOwnRequestIntent(store, account, group, RetryableRequest.SetAdmin(alice, true))
        assertEquals(1, loadOwnRequestIntents(store, account, group).size)
        forgetOwnRequestIntent(store, account, group, RetryableRequest.SetAdmin(bob, false))
        assertTrue(loadOwnRequestIntents(store, account, group).isEmpty())
    }

    @Test
    fun `a stopped request is listed under its own kind`() {
        assertEquals(UnappliedKind.ADD, unappliedFor(RetryableRequest.Add(carol, aliceAccount)).kind)
        assertEquals(UnappliedKind.JOIN_APPROVE, unappliedFor(RetryableRequest.ApproveJoin(carol)).kind)
        assertEquals(UnappliedKind.REMOVE, unappliedFor(RetryableRequest.Remove(bob)).kind)
        assertEquals(UnappliedKind.LEAVE, unappliedFor(RetryableRequest.Leave(me)).kind)
        assertEquals(UnappliedKind.MAKE_ADMIN, unappliedFor(RetryableRequest.SetAdmin(alice, true)).kind)
        assertEquals(UnappliedKind.REMOVE_ADMIN, unappliedFor(RetryableRequest.SetAdmin(alice, false)).kind)
        assertEquals(
            UnappliedKind.TRANSFER_OWNERSHIP,
            unappliedFor(RetryableRequest.TransferOwnership(alice, OutgoingOwner.ADMIN)).kind,
        )
    }
}
