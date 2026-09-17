// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.ProcessedMessage
import uniffi.lakko_ffi_uniffi.ProposalKind
import uniffi.lakko_ffi_uniffi.ProposalRecord

/**
 * ADR-0045 UX §10 / §4 — a received membership proposal is a PANEL signal
 * with a DS timestamp, never a timeline row.
 *
 * ## The two defects this pins, and the second is the one that matters
 *
 * `toEvent` mapped `ProcessedMessage.Proposal` onto
 * `IncomingChatEvent.System(SystemKind.Proposal)`, which `consumeEvents`
 * rendered as a literal `[mls proposal]` bubble. §10 is explicit that this
 * *"stops being a **timeline** row and becomes a **panel** signal"*: a request
 * is not something that happened to the group, it is an intent the system may
 * discard a second later, and writing it into a permanent mutually-visible
 * transcript is both a durable record of a discarded intent and the §10 leak.
 *
 * The stray bubble is the visible half. The half that is not visible is that
 * the arm carried **no payload**, so nothing downstream could stamp an
 * observation — and `recordPendingRequestObservation`'s only production caller
 * was the OWN-request path. A request arriving from somebody else therefore
 * never got a DS timestamp, `requestState` read `observedAtMs == null`, and by
 * its own documented rule that means `WAITING` **immediately**. So the §4
 * quiet window — which exists precisely so a request that resolves in
 * milliseconds never flashes an annotation — silently never applied to any
 * request the reader did not make themselves, which is every request an admin
 * ever sees from a peer.
 *
 * ## Why the store is faked and the rest is real
 *
 * Plain JVM `test` source set: `liblakko_ffi_uniffi.so` does not exist here.
 * `ProposalRecord` / `ProcessedMessage.Proposal` are generated *data* types
 * and construct fine without it; only the store is a double, and it is the
 * same [PendingRequestStore] interface production injects.
 *
 * ## Which rules here actually discriminate (MEASURED — each probe was run)
 *
 * Three deliberate regressions, each applied, `:app:testDebugUnitTest` run,
 * the output below observed verbatim, and the probe reverted.
 *
 * **A. The defect itself restored** — `toEvent` maps `Proposal` back onto
 * `System`, and `SystemKind.Proposal` comes back with it (they have to move
 * together; the enum entry alone does not compile away):
 *
 *     MembershipProposalTest > a received proposal becomes a panel signal
 *       carrying its record FAILED
 *       java.lang.AssertionError: a received proposal must be a panel signal,
 *       not a timeline row, but got System(messageId=7, systemKind=Proposal,
 *       createdAt=2026-08-31T10:00:00Z)
 *     MembershipProposalTest > no SystemKind can name a proposal any more FAILED
 *       java.lang.AssertionError: expected:<[Commit, ExternalJoinProposal]>
 *       but was:<[Commit, Proposal, ExternalJoinProposal]>
 *
 * **B. `recordReceivedProposalObservation` drops its `stored` guard and stamps
 * `own = true`** — the first records a proposal the receipt gate refused, the
 * second would leak a peer's request to a plain member through §10's
 * own-request exception:
 *
 *     MembershipProposalTest > a received proposal is stamped with the DS row
 *       time, not ours FAILED
 *       java.lang.AssertionError: a peer's request is not ours
 *     MembershipProposalTest > a refused proposal is stamped nowhere FAILED
 *       java.lang.AssertionError
 *
 * **C. It falls back to `System.currentTimeMillis()` when the DS timestamp
 * does not parse:**
 *
 *     MembershipProposalTest > an unparseable DS timestamp stamps nothing
 *       rather than guessing FAILED
 *       java.lang.AssertionError
 *
 * Two of the five failures carry no message because the assertion is a bare
 * `assertFalse` / `assertTrue` on a boolean; the test name is what identifies
 * them, which is why the names above are sentences.
 */
class MembershipProposalTest {

    private class MemPendingStore : PendingRequestStore {
        val rows = mutableMapOf<String, Map<String, RequestObservation>>()
        private fun k(a: String, g: String) = "$a\u0000$g"
        override fun load(accountIdB64: String, groupIdHex: String) =
            rows[k(accountIdB64, groupIdHex)] ?: emptyMap()

        override fun save(
            accountIdB64: String,
            groupIdHex: String,
            observations: Map<String, RequestObservation>,
        ) {
            rows[k(accountIdB64, groupIdHex)] = observations
        }

        override fun clearForAccount(accountIdB64: String) {
            rows.keys.removeAll { it.startsWith("$accountIdB64\u0000") }
        }
    }

    private val account = "acct-A"
    private val group = "aabb"

    private fun removeRecord(leaf: UInt = 4u, stored: Boolean = true) =
        ProposalRecord(senderLeaf = 2u, kind = ProposalKind.Remove(leaf), stored = stored)

    // -----------------------------------------------------------------
    // §10 — a panel signal, and no timeline row
    // -----------------------------------------------------------------

    @Test
    fun `a received proposal becomes a panel signal carrying its record`() {
        val record = removeRecord()

        val event = toEvent(
            messageId = 7L,
            createdAt = "2026-08-31T10:00:00Z",
            processed = ProcessedMessage.Proposal(record),
            resolveSender = { null },
            resolveLeafSignatureKey = { null },
        )

        assertTrue(
            "a received proposal must be a panel signal, not a timeline row, " +
                "but got $event",
            event is IncomingChatEvent.MembershipProposal,
        )
        val signal = event as IncomingChatEvent.MembershipProposal
        assertEquals(7L, signal.messageId)
        assertEquals("2026-08-31T10:00:00Z", signal.createdAt)
        // The record travels WHOLE. Without it nothing downstream can compute
        // `proposalKey`, and without the key there is nothing to stamp.
        assertEquals(record, signal.record)
    }

    @Test
    fun `no SystemKind can name a proposal any more`() {
        // The strongest available form of "never a timeline row": `System` is
        // the only variant `consumeEvents` turns into a `ChatMessage`, and
        // `SystemKind` is a closed enum. If nothing in it names a proposal,
        // no proposal can reach the transcript by any route — including one a
        // later change adds without reading §10.
        assertEquals(
            listOf(SystemKind.Commit, SystemKind.ExternalJoinProposal),
            SystemKind.entries.toList(),
        )
    }

    @Test
    fun `a commit is still a timeline row`() {
        // The counterweight: §10 draws the line at outcomes, and a commit IS
        // one. A change that swept every membership envelope off the timeline
        // would pass the test above and be wrong.
        val event = toEvent(
            messageId = 9L,
            createdAt = "2026-08-31T10:00:01Z",
            processed = ProcessedMessage.Commit,
            resolveSender = { null },
            resolveLeafSignatureKey = { null },
        )

        assertEquals(
            IncomingChatEvent.System(9L, SystemKind.Commit, "2026-08-31T10:00:01Z"),
            event,
        )
    }

    // -----------------------------------------------------------------
    // §4 — the observation, stamped with the DS row's own timestamp
    // -----------------------------------------------------------------

    @Test
    fun `a received proposal is stamped with the DS row time, not ours`() {
        // "`age` is measured against the DS row timestamp, never a local
        // clock: every member reads the identical value for the identical
        // row." That is what makes the quiet window mean the same thing on
        // every device rather than being a function of clock skew.
        val store = MemPendingStore()
        val record = removeRecord()

        val recorded = recordReceivedProposalObservation(
            store = store,
            accountIdB64 = account,
            groupIdHex = group,
            record = record,
            atMs = 1_700_000_000_000L,
        )

        assertTrue(recorded)
        val observation = store.load(account, group)[proposalKey(record)]
        assertEquals(1_700_000_000_000L, observation?.atMs)
        assertFalse("a peer's request is not ours", observation?.own ?: true)
        assertNull("only the publisher supplies a successor", observation?.transferToB64)
    }

    @Test
    fun `the stamp makes the quiet window apply to a peer's request`() {
        // The consequence, asserted rather than left implied. An unstamped
        // request reads WAITING immediately by `requestState`'s own documented
        // rule, so before the sink existed every peer request flashed its
        // annotation the instant it landed.
        val store = MemPendingStore()
        val record = removeRecord()
        val landedAt = 1_700_000_000_000L

        assertEquals(
            RequestState.WAITING,
            requestState(observedAtMs = null, nowMs = landedAt),
        )

        recordReceivedProposalObservation(store, account, group, record, landedAt)
        val stamped = store.load(account, group)[proposalKey(record)]?.atMs

        assertEquals(
            RequestState.PENDING,
            requestState(observedAtMs = stamped, nowMs = landedAt + 1L),
        )
        assertEquals(
            RequestState.WAITING,
            requestState(observedAtMs = stamped, nowMs = landedAt + QUIET_WINDOW_MS),
        )
    }

    @Test
    fun `a refused proposal is stamped nowhere`() {
        // `stored == false` is the receipt gate's refusal reported as data. No
        // sweep will ever commit it, so it is not a pending request and must
        // leave no residue in a store that is pruned against the live set.
        val store = MemPendingStore()

        val recorded = recordReceivedProposalObservation(
            store, account, group, removeRecord(stored = false), 1_700_000_000_000L,
        )

        assertFalse(recorded)
        assertTrue(store.load(account, group).isEmpty())
    }

    @Test
    fun `an unparseable DS timestamp stamps nothing rather than guessing`() {
        // The alternative — fall back to the local clock — is worse than not
        // stamping: an unstamped request renders WAITING immediately, which is
        // merely eager, while a locally-stamped one silently reintroduces the
        // clock skew the DS timestamp exists to remove, and does it invisibly.
        val store = MemPendingStore()

        val recorded = recordReceivedProposalObservation(
            store, account, group, removeRecord(), atMs = null,
        )

        assertFalse(recorded)
        assertTrue(store.load(account, group).isEmpty())
    }

    @Test
    fun `observing our own request land back does not clear the own flag`() {
        // The receive loop sees our own proposal echo off the journal, and
        // that observation carries `own = false`. First-wins on `atMs` plus
        // monotone `own` is what keeps §10's non-negotiable own-request
        // exception from being lost to arrival order.
        val store = MemPendingStore()
        val record = removeRecord()
        recordPendingRequestObservation(
            store, account, group, proposalKey(record), atMs = 500L, own = true,
        )

        recordReceivedProposalObservation(store, account, group, record, atMs = 900L)

        val observation = store.load(account, group)[proposalKey(record)]
        assertEquals(500L, observation?.atMs)
        assertTrue("the own flag must survive the echo", observation?.own ?: false)
    }
}
