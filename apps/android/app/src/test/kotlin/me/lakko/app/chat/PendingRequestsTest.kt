// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.ProposalKind
import uniffi.lakko_ffi_uniffi.ProposalRecord

/**
 * ADR-0045 S4a — the pending-membership-request surface, as
 * `docs/ux/proposal-based-membership.md` specifies it.
 *
 * Mirrors `apps/web/src/chat/pending-requests.test.ts`. Every rule the UX spec
 * states lives in `PendingRequests.kt` rather than in the composable precisely so
 * it can be run here.
 *
 * ## Which of these discriminate (each break was RUN; quoted verbatim)
 *
 * Including a record the receipt gate REFUSED (`stored == false`):
 *   - `a refused proposal is not a pending request`:
 *     `expected:<0> but was:<1>`.
 *
 * Showing the whole pending set to a plain member:
 *   - `a plain member sees only its own request`:
 *     `expected:<[remove:4]> but was:<[add:AQID, remove:4]>`.
 *
 * Letting a later observation reset a request's age (`recordPendingRequestObservation`
 * writing `atMs` instead of `existing?.atMs ?: atMs`):
 *   - `the first observation of a request wins`: `expected:<100> but was:<900>`.
 */
class PendingRequestsTest {

    private class MemStore : PendingRequestStore {
        val rows = mutableMapOf<String, MutableMap<String, RequestObservation>>()

        override fun load(accountIdB64: String, groupIdHex: String) =
            rows["$accountIdB64/$groupIdHex"]?.toMap() ?: emptyMap()

        override fun save(
            accountIdB64: String,
            groupIdHex: String,
            observations: Map<String, RequestObservation>,
        ) {
            rows["$accountIdB64/$groupIdHex"] = observations.toMutableMap()
        }

        override fun clearForAccount(accountIdB64: String) {
            rows.keys.filter { it.startsWith("$accountIdB64/") }.forEach { rows.remove(it) }
        }
    }

    private fun add(key: ByteArray, sender: UInt? = 1u, stored: Boolean = true) =
        ProposalRecord(senderLeaf = sender, kind = ProposalKind.Add(key), stored = stored)

    private fun remove(leaf: UInt, sender: UInt? = 1u, stored: Boolean = true) =
        ProposalRecord(senderLeaf = sender, kind = ProposalKind.Remove(leaf), stored = stored)

    /**
     * The receipt gate's refusal is reported as DATA, not as a throw — a proposal
     * advances no state, so dropping it unstored IS the refusal. A refused record
     * must never render: no sweep will ever commit it.
     */
    @Test
    fun `a refused proposal is not a pending request`() {
        val folded = foldPendingRequests(
            records = listOf(remove(leaf = 4u, stored = false)),
            observations = emptyMap(),
            nowMs = 10_000,
        )
        assertEquals(0, folded.requests.size)
        assertEquals(0, folded.liveKeys.size)
    }

    /**
     * The quiet window (UX §12.1). The owner is rank 0 and sweeps its own request
     * immediately, so an affordance that appears and vanishes in 300 ms teaches
     * the user that membership is slow when it is not.
     */
    @Test
    fun `the quiet window suppresses a just-landed request`() {
        assertEquals(RequestState.PENDING, requestState(observedAtMs = 10_000, nowMs = 11_000))
        assertEquals(RequestState.WAITING, requestState(observedAtMs = 10_000, nowMs = 14_000))
        // An UNOBSERVED request was pending since before this session started, so
        // it is by definition past any window. Treating it as PENDING would hide
        // a genuinely old request behind a timer that never fires.
        assertEquals(RequestState.WAITING, requestState(observedAtMs = null, nowMs = 0))
    }

    /**
     * UX §10: the pending set is a work queue and only owner/admins have a verb
     * over it. Showing a plain member "Bob is waiting to be removed" tells them
     * something they cannot act on, about someone else's standing, in a window
     * that did not exist before ADR-0045 created it. The own-request exception is
     * non-negotiable — without it a self-remove is an action with no feedback.
     */
    @Test
    fun `a plain member sees only its own request`() {
        val store = MemStore()
        recordPendingRequestObservation(store, "acct", "grp", "remove:4", 100, own = true)
        val folded = foldPendingRequests(
            records = listOf(add(byteArrayOf(1, 2, 3)), remove(leaf = 4u)),
            observations = store.load("acct", "grp"),
            nowMs = 10_000,
        )
        assertEquals(
            listOf("remove:4"),
            visibleRequests(folded.requests, canManageMembers = false).map { it.key },
        )
        assertEquals(
            listOf("add:AQID", "remove:4"),
            visibleRequests(folded.requests, canManageMembers = true).map { it.key },
        )
    }

    /**
     * The FIRST observation wins — the deliberate opposite of the beacon store,
     * where recency is the signal. A request that keeps being re-sent must not
     * keep resetting its age, or it would stay invisible forever, which is exactly
     * the wait the notice exists to surface.
     */
    @Test
    fun `the first observation of a request wins`() {
        val store = MemStore()
        recordPendingRequestObservation(store, "acct", "grp", "remove:4", 100, own = false)
        recordPendingRequestObservation(store, "acct", "grp", "remove:4", 900, own = true)
        val row = store.load("acct", "grp")["remove:4"]!!
        assertEquals(100L, row.atMs)
        // ...but `own` is UPGRADED, so the visibility exception is not lost to
        // arrival order.
        assertTrue("own must survive a later self-publish", row.own)
    }

    /** The §5.1 copy. ONE liveness line, not an escalating pair: ADR-0045 gives a
     *  waiting request no deadline, so there is no moment at which the client
     *  learns something new to say. */
    @Test
    fun `the pending notice states inaction and blames nobody`() {
        assertNull("nothing waiting renders no notice", pendingNotice(emptyList()))
        val folded = foldPendingRequests(
            records = listOf(remove(leaf = 4u)),
            observations = mapOf("remove:4" to RequestObservation(atMs = 0, own = false)),
            nowMs = 10_000,
        )
        val notice = pendingNotice(folded.requests)!!
        assertEquals("1 membership change is waiting to apply.", notice.count)
        assertEquals(
            "Membership changes are applied by the group owner or an admin. " +
                "They will apply automatically when one does.",
            notice.liveness,
        )
    }

    /**
     * NO optimistic update: a member with a pending removal STAYS in the roster,
     * annotated. Dropping the row would assert they are gone while they are still
     * cryptographically in the group.
     */
    @Test
    fun `a pending removal annotates the row rather than dropping it`() {
        val folded = foldPendingRequests(
            records = listOf(remove(leaf = 4u)),
            observations = mapOf("remove:4" to RequestObservation(atMs = 0, own = false)),
            nowMs = 10_000,
        )
        assertEquals(
            "Waiting to be removed.",
            rowAnnotation(folded.requests, leafIndex = 4u, ownLeaf = 1u),
        )
        // Targeting the reader's OWN leaf is how leaving works.
        assertEquals(
            "Waiting to leave.",
            rowAnnotation(folded.requests, leafIndex = 4u, ownLeaf = 4u),
        )
        assertNull(rowAnnotation(folded.requests, leafIndex = 9u, ownLeaf = 1u))
        // Below the quiet window nothing is annotated at all.
        val young = foldPendingRequests(
            records = listOf(remove(leaf = 4u)),
            observations = mapOf("remove:4" to RequestObservation(atMs = 9_500, own = false)),
            nowMs = 10_000,
        )
        assertNull(rowAnnotation(young.requests, leafIndex = 4u, ownLeaf = 1u))
    }

    /** A person who has been REQUESTED is not a member, so they must not appear
     *  in the roster — they get their own section. */
    @Test
    fun `a pending add gets its own section`() {
        val folded = foldPendingRequests(
            records = listOf(add(byteArrayOf(1, 2, 3)), remove(leaf = 4u)),
            observations = mapOf(
                "add:AQID" to RequestObservation(atMs = 0, own = false),
                "remove:4" to RequestObservation(atMs = 0, own = false),
            ),
            nowMs = 10_000,
        )
        assertEquals(listOf("add:AQID"), waitingToBeAdded(folded.requests).map { it.key })
    }

    /**
     * The key is derived from what the request IS, never from a leaf index alone
     * where a signature key is available: OpenMLS reuses a vacated leaf for the
     * next Add, so a leaf-keyed record can silently come to name someone else.
     *
     * `role-change` collapses to a single key on purpose — `classify` discards the
     * `GroupContextExtensions` payload, so two of them are indistinguishable
     * (UX §11.5).
     */
    @Test
    fun `the request key names the request not the leaf`() {
        assertEquals("add:AQID", proposalKey(add(byteArrayOf(1, 2, 3))))
        assertEquals("remove:4", proposalKey(remove(leaf = 4u)))
        assertEquals(
            "role-change",
            proposalKey(ProposalRecord(senderLeaf = 2u, kind = ProposalKind.RoleChange, stored = true)),
        )
        assertEquals(
            "leave:3",
            proposalKey(ProposalRecord(senderLeaf = 3u, kind = ProposalKind.Leave, stored = true)),
        )
    }

    /** A swept or superseded request must leave no residue. */
    @Test
    fun `observations are pruned against the live set`() {
        val store = MemStore()
        recordPendingRequestObservation(store, "acct", "grp", "remove:4", 100, own = false)
        recordPendingRequestObservation(store, "acct", "grp", "add:AQID", 100, own = false)
        prunePendingRequestObservations(store, "acct", "grp", liveKeys = listOf("remove:4"))
        assertEquals(setOf("remove:4"), store.load("acct", "grp").keys)
    }

    /** The `SharedPreferences` row codec, round-tripped without a `Context`. */
    @Test
    fun `the observation row codec round trips`() {
        val observations = mapOf(
            "add:AQID" to RequestObservation(atMs = 1_700_000_000_123L, own = true),
            "remove:4" to RequestObservation(atMs = 42L, own = false),
        )
        val encoded = SharedPrefsPendingRequestStore.encodeRow(observations)
        assertEquals(observations, SharedPrefsPendingRequestStore.decodeRow(encoded))
        assertEquals(
            emptyMap<String, RequestObservation>(),
            SharedPrefsPendingRequestStore.decodeRow(null),
        )
        assertEquals(
            emptyMap<String, RequestObservation>(),
            SharedPrefsPendingRequestStore.decodeRow("garbage"),
        )
    }
}
