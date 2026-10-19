// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The receive path's DURABILITY ORDERING — the journal cursor may LAG the
 * durable MLS state, never LEAD it. Android mirror of the web
 * `chat/receive-durability.test.ts` and the iOS `ReceiveDurabilityTests.swift`.
 *
 * ## The defect these pin
 *
 * ADR-0045 made a receiver's durable proposal store a PRECONDITION for applying
 * a future by-reference commit: OpenMLS resolves the reference against the
 * receiver's own store, so a peer that lost the proposal fails
 * `InvalidCommit(MissingProposal)` before any Lakko gate runs. That is correct
 * for a proposal the gate REFUSED and fatal for one the device LOST.
 *
 * Losing one took no adversary. The receive path's snapshot write was
 * best-effort (`persistSnapshotBestEffort`) and swallowed its failures, the
 * cursor lives inside a different file (the [MessageLogStore] envelope under
 * `noBackupFilesDir`) so the two writes can never be atomic, and the cursor
 * advanced regardless. A swallowed filesystem error — or a crash between the two
 * writes — left the device with an MLS state that never saw the proposal and a
 * cursor already past it. The committer's commit then threw, was swallowed by
 * the per-row catch, the cursor advanced past THAT too, and the device sat at
 * epoch N forever while the group moved to N+1. `isActive()` does not catch it:
 * the device is still a member, just epoch-stranded.
 *
 * ## What is driven
 *
 * [applyDurableCursorBatch] — the REAL production ordering function, the same
 * one `ChatRepository.pollGroup` and the ADR-0045 verdict apply call. The
 * repository needs an Android `Context` and live FFI handles, so the ordering it
 * performs is extracted, exactly as [runSplitPoll] and [prepareCrashSafeCommit] are;
 * a test that re-implemented the sequence would prove nothing about production.
 *
 * The device around it models its DURABLE state separately from its in-memory
 * state, so a restart is a real operation rather than an assertion about one,
 * and reproduces the one OpenMLS behaviour the whole defect turns on: a commit
 * that references a proposal THROWS unless that proposal is stored.
 *
 * ## Measured discrimination — the probe was RUN, failure quoted verbatim
 *
 * With [applyDurableCursorBatch] reverted to the pre-fix shape (swallow the
 * persist, advance unconditionally):
 *
 *     ReceiveDurabilityTest > a lost proposal strands the device forever FAILED
 *         java.lang.AssertionError: the cursor must not pass a batch whose
 *         persist failed expected:<0> but was:<10>
 *     ReceiveDurabilityTest > no interleaving can ever strand the device FAILED
 *         java.lang.AssertionError: a crash before consuming Commit(needs=A)
 *         would strand the device (cursor 10)
 *     ReceiveDurabilityTest > a held batch is refetched until a write lands
 *         FAILED  java.lang.AssertionError: expected:<HELD> but was:<ADVANCED>
 *     ReceiveDurabilityTest > the good path costs one persist per batch and none
 *         when empty FAILED  java.lang.AssertionError: an empty batch persists
 *         nothing expected:<0> but was:<1>
 *     ReceiveDurabilityTest > an all-echo batch touches nothing and persists
 *         nothing FAILED  java.lang.AssertionError: an echo row never reaches the
 *         core, so it cannot have mutated anything expected:<0> but was:<1>
 *
 * The sticky-flag half of the gate has NO test, and that is deliberate: no
 * reachable case was found. Holding the cursor is what makes the touching rows
 * be re-fetched, and a row the echo filter did not drop is handed to the core
 * again — so the gate re-marks itself and the flag never changes an outcome
 * under the current call shape. It is kept as defence in depth; see
 * [DurableCursorGate].
 */
class ReceiveDurabilityTest {

    /** One DS journal row. */
    private sealed interface Row {
        /** A membership proposal the receiver must STORE to later resolve a
         *  commit that carries it by reference. */
        data class Proposal(val ref: String) : Row

        /** A commit carrying [needs] BY REFERENCE — throws if it is not stored. */
        data class Commit(val needs: String) : Row

        /** A row nothing can ever apply — corrupt frame, wrong-epoch duplicate,
         *  non-MLS envelope. */
        data object Junk : Row

        /** One of OUR OWN published rows: dropped by the echo filter BEFORE it
         *  reaches the core, so it can have mutated nothing. */
        data object Echo : Row
    }

    /**
     * A device whose durable MLS snapshot is modelled apart from its live state.
     */
    private class Device {
        var durableProposals: Set<String> = emptySet()
            private set
        var durableEpoch: Int = 0
            private set
        private var memProposals: MutableSet<String> = mutableSetOf()
        var memEpoch: Int = 0
            private set
        var cursor: Long = 0
            private set
        var persistCalls: Int = 0
            private set
        var persistFails: Boolean = false
        private val gate = DurableCursorGate()

        private val journal = mutableListOf<Pair<Long, Row>>()
        private var nextId: Long = 9

        fun push(row: Row): Long {
            nextId += 1
            journal.add(nextId to row)
            return nextId
        }

        fun rowsAfter(after: Long): List<Pair<Long, Row>> = journal.filter { it.first > after }

        /** The production sequence: apply the batch in memory, then run the ONE
         *  ordering function the repository runs. */
        fun poll(): CursorVerdict {
            var touched = false
            var last = cursor
            for ((id, row) in rowsAfter(cursor)) {
                last = id
                if (row is Row.Echo) continue
                touched = true
                when (row) {
                    is Row.Junk, is Row.Echo -> Unit
                    is Row.Proposal ->
                        // `lakko-mls` de-duplicates by `ProposalRef` before
                        // storing, so a re-delivered proposal is idempotent.
                        memProposals.add(row.ref)
                    is Row.Commit ->
                        if (memProposals.contains(row.needs)) {
                            memEpoch += 1
                            memProposals.clear()
                        }
                }
            }
            return applyDurableCursorBatch(
                gate = gate,
                touchedMlsState = touched,
                persist = {
                    persistCalls += 1
                    if (persistFails) throw java.io.IOException("no space left on device")
                    durableProposals = memProposals.toSet()
                    durableEpoch = memEpoch
                },
                advance = { cursor = last },
            )
        }

        /** Discard everything in memory and reload from disk. */
        fun restart() {
            memProposals = durableProposals.toMutableSet()
            memEpoch = durableEpoch
        }

        /**
         * Would a crash RIGHT NOW leave the device unable to apply a later
         * commit? Replays the journal from the durable cursor against the
         * durable proposal set — exactly what the next launch would do.
         */
        fun strandedAfterCrash(): Boolean {
            val held = durableProposals.toMutableSet()
            for ((_, row) in rowsAfter(cursor)) {
                when (row) {
                    is Row.Proposal -> held.add(row.ref)
                    is Row.Commit -> {
                        if (!held.contains(row.needs)) return true
                        held.clear()
                    }
                    is Row.Junk, is Row.Echo -> Unit
                }
            }
            return false
        }
    }

    /** THE HEADLINE. Everything else in this file is a guard around it. */
    @Test
    fun `a lost proposal strands the device forever`() {
        val d = Device()
        val proposalId = d.push(Row.Proposal("P"))

        // The proposal arrives while the snapshot store is broken.
        d.persistFails = true
        val verdict = d.poll()
        assertEquals(10L, proposalId)
        // The rule first, because it is the rule: the cursor MUST NOT have moved
        // past a batch whose durable write did not land.
        assertEquals(
            "the cursor must not pass a batch whose persist failed",
            0L,
            d.cursor,
        )
        assertEquals(CursorVerdict.HELD, verdict)
        assertTrue(d.durableProposals.isEmpty())

        // Meanwhile the elected committer sweeps it into a commit that carries
        // it BY REFERENCE — the artifact this device can only apply if it kept
        // the proposal.
        d.push(Row.Commit(needs = "P"))

        // The storage failure clears, and the app is relaunched.
        d.persistFails = false
        d.restart()
        assertEquals(0, d.memEpoch)

        // The poll resumes from the DURABLE cursor, which is where the durable
        // MLS state is, so the proposal is re-delivered ahead of the commit.
        assertEquals(CursorVerdict.ADVANCED, d.poll())
        assertEquals(
            "the device must still apply the commit after a restart",
            1,
            d.memEpoch,
        )
        assertEquals(1, d.durableEpoch)
        assertEquals(11L, d.cursor)
    }

    /**
     * A held batch is RE-FETCHED, and the cursor only catches up once a write
     * lands — through however many failures it takes.
     */
    @Test
    fun `a held batch is refetched until a write lands`() {
        val d = Device()
        d.push(Row.Proposal("P"))
        d.persistFails = true
        assertEquals(CursorVerdict.HELD, d.poll())
        assertEquals(0L, d.cursor)

        d.push(Row.Echo)
        assertEquals(CursorVerdict.HELD, d.poll())
        assertEquals(
            "the cursor must not creep forward while the snapshot is behind",
            0L,
            d.cursor,
        )
        assertEquals("and every attempt retries the write", 2, d.persistCalls)

        // The store recovers: the next batch persists and the cursor catches up
        // in one step, over everything held back.
        d.persistFails = false
        d.push(Row.Junk)
        assertEquals(CursorVerdict.ADVANCED, d.poll())
        assertEquals(12L, d.cursor)
        assertEquals(setOf("P"), d.durableProposals)
    }

    /**
     * The trap the ordering rule could have set. Only a PERSIST failure holds the
     * cursor; a row that can never apply is skipped exactly as before, so the
     * poll cannot loop on it forever.
     */
    @Test
    fun `a permanently undecryptable row never wedges the poll`() {
        val d = Device()
        d.push(Row.Junk)
        d.push(Row.Junk)
        assertEquals(CursorVerdict.ADVANCED, d.poll())
        assertEquals(11L, d.cursor)
        assertEquals(CursorVerdict.ADVANCED, d.poll())
        assertEquals(11L, d.cursor)
    }

    @Test
    fun `the good path costs one persist per batch and none when empty`() {
        val d = Device()
        assertEquals(CursorVerdict.ADVANCED, d.poll())
        assertEquals("an empty batch persists nothing", 0, d.persistCalls)

        d.push(Row.Proposal("P"))
        d.push(Row.Commit(needs = "P"))
        assertEquals(CursorVerdict.ADVANCED, d.poll())
        assertEquals(1, d.persistCalls)
        assertEquals(11L, d.cursor)
    }

    @Test
    fun `an all-echo batch touches nothing and persists nothing`() {
        val d = Device()
        d.push(Row.Echo)
        assertEquals(CursorVerdict.ADVANCED, d.poll())
        assertEquals(
            "an echo row never reaches the core, so it cannot have mutated anything",
            0,
            d.persistCalls,
        )
        assertEquals(10L, d.cursor)
    }

    @Test
    fun `no interleaving can ever strand the device`() {
        val d = Device()
        // The failure pattern is EXPLICIT, not alternating. Alternating happens
        // to let a successful write land between a proposal and the commit that
        // references it — and a successful write captures the still-live
        // in-memory state, repairing the gap before the replay can see it. The
        // hazard needs a commit arriving while the durable snapshot is behind,
        // so the pairs below are (row, does this batch's write fail).
        val seq = listOf(
            Row.Proposal("A") to true,
            Row.Commit("A") to true,
            Row.Junk to false,
            Row.Proposal("B") to true,
            Row.Echo to true,
            Row.Commit("B") to false,
            Row.Proposal("C") to true,
            Row.Commit("C") to true,
        )
        for ((row, failWrite) in seq) {
            d.push(row)
            d.persistFails = failWrite
            // Checked BEFORE the row is consumed, which is the only moment it can
            // be caught: once a commit has been polled — applied or thrown — it is
            // no longer above the cursor and the replay has nothing to fail on.
            assertFalse(
                "a crash before consuming $row would strand the device (cursor ${d.cursor})",
                d.strandedAfterCrash(),
            )
            d.poll()
        }
    }
}
