// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0045 S4a/S4b — the five sweep obligations, driven end to end over the
 * ports.
 *
 * Mirrors `apps/web/src/chat/sweep.test.ts` in coverage, not in shape: the FFI
 * `.so` is Android-only, so where the web suite wires the REAL wasm through its
 * ports this one wires a `FakeGroup` that models the staged pair exactly as the
 * core implements it — staging leaves the epoch alone, merging advances it,
 * abandoning does not, and applying a foreign commit advances it. The rules the
 * fake stands in for are proven by 180 `#[test]`s in `lakko-mls`; what is proven
 * HERE is the composition around them, which is where every one of the five
 * obligations lives.
 *
 * ## Which of these discriminate (each break was RUN; the failures below are
 * quoted verbatim from that run, not predicted)
 *
 * Publishing the Welcome BEFORE the verdict (`publishWelcomes` moved above
 * `adjudicate`) — four tests fail, and the two that name the obligation are:
 *   - `welcome is not published before the verdict`:
 *     `a joiner must never open into a state the group does not hold
 *     expected:<[]> but was:<[mailbox@2]>`;
 *   - `a losing sweep abandons and the proposals survive`:
 *     `expected:<[commit@1]> but was:<[commit@1, mailbox@2]>`.
 *
 * Moving the `hasStagedSweep` check BELOW the rank check:
 *   - `an owed verdict is resolved even after a demotion`:
 *     `outcome was NotCommitter(rank=null)` — and the group stays frozen.
 *
 * Dropping the stand-down re-read (`shouldStandDown` -> `false`):
 *   - `an unapplied row stands the sweep down`:
 *     `outcome was Committed(commitMessageId=1, welcomeMessageIds=[2], addedCount=1)`.
 *
 * Publishing BEFORE the durable record:
 *   - `a failed durable record aborts before any publish`:
 *     `expected:<[]> but was:<[commit@1]>`;
 *   - `the durable record lands before any publish`:
 *     `expected:<[record, publish]> but was:<[publish, record, publish]>`.
 *
 * Treating a `null` committer rank as the owner (`Candidate(rank ?: 0u)`):
 *   - `a group with no role table is never swept`:
 *     `outcome was Committed(commitMessageId=1, welcomeMessageIds=[2], addedCount=1)`.
 *
 * Taking the provider lock PER ROW instead of around the whole adjudication:
 *   - `the adjudication apply is one provider region`:
 *     `the whole apply loop must be ONE provider region
 *     expected:<[enter, exit, enter, exit, enter, apply, apply, apply, exit,
 *     enter, exit]> but was:<[enter, exit, enter, exit, enter, exit, enter,
 *     apply, exit, enter, apply, exit, enter, apply, exit, enter, exit, enter,
 *     exit]>`.
 */
class CommitterSweepTest {

    // ---------------------------------------------------------------- doubles

    /** A stand-in for the MLS group that models the staged pair the way the core
     *  implements it. Also records the provider-lock bracketing so the
     *  adjudication region's atomicity is assertable. */
    private class FakeGroup(
        var epoch: ULong = 7uL,
        var rank: UInt? = 0u,
        var pending: Int = 1,
        val staged: StagedSweepArtifacts? = StagedSweepArtifacts(
            commit = byteArrayOf(1),
            welcome = byteArrayOf(2),
            addedSignatureKeys = listOf(ByteArray(32) { 9 }),
        ),
    ) : SweepGroupPort {
        var hasStaged: Boolean = false
        var merged: Int = 0
        var abandoned: Int = 0
        val applied = mutableListOf<Int>()
        val providerTrace = mutableListOf<String>()

        /** Rows whose payload byte is in here advance the epoch when applied. */
        val foreignCommits = mutableSetOf<Int>()

        override suspend fun <T> exclusiveProvider(block: suspend () -> T): T {
            providerTrace.add("enter")
            try {
                return block()
            } finally {
                providerTrace.add("exit")
            }
        }

        override fun epoch(): ULong = epoch

        override fun ownCommitterRank(): UInt? = rank

        override fun pendingProposalCount(): Int = pending

        override fun hasStagedSweep(): Boolean = hasStaged

        override fun stagePendingProposals(): StagedSweepArtifacts? {
            check(!hasStaged) { "the core refuses a second stage" }
            if (staged == null || pending == 0) return null
            hasStaged = true
            return staged
        }

        override fun mergeStagedSweep() {
            check(hasStaged)
            hasStaged = false
            merged++
            epoch = epoch + 1uL
            // OpenMLS empties the proposal store on every merged commit.
            pending = 0
        }

        override fun abandonStagedSweep() {
            // The real core returns `StagedSweepOutcome.NOTHING_STAGED` rather
            // than throwing when nothing is staged — reachable here because
            // applying a foreign commit already discarded the pending one.
            hasStaged = false
            abandoned++
            // The proposals SURVIVE for the winner's next sweep.
        }

        override fun applyIncoming(payload: ByteArray) {
            val tag = payload[0].toInt()
            applied.add(tag)
            providerTrace.add("apply")
            if (tag !in foreignCommits) throw IllegalStateException("not an applyable commit")
            // Applying a foreign commit discards our pending one.
            if (hasStaged) hasStaged = false
            epoch = epoch + 1uL
        }
    }

    /**
     * A DS double whose fetches are SCRIPTED in order, because the two entry
     * points ask a different number of questions: `runCommitterSweep` fetches
     * twice (the obligation-3 stand-down re-read, then the verdict read) while
     * `resolveOwedVerdict` fetches once (the verdict read only). A double that
     * answered by call index would silently feed the stand-down's answer to a
     * restart's verdict.
     */
    private class FakeJournal(
        /** The default answer, once [scripted] is exhausted. */
        var rows: List<SweepJournalRow> = emptyList(),
        /** Answers for the leading fetches, in order. */
        val scripted: MutableList<List<SweepJournalRow>> = mutableListOf(),
    ) : SweepJournalPort {
        val published = mutableListOf<String>()
        var nextId: Long = 1
        var fetches = 0

        override suspend fun fetch(after: Long): List<SweepJournalRow> {
            fetches++
            return if (scripted.isNotEmpty()) scripted.removeAt(0) else rows
        }

        override suspend fun publishGroup(bytes: ByteArray): Long {
            published.add("commit@${bytes[0].toInt()}")
            return nextId++
        }

        override suspend fun publishMailbox(pseudonym: ByteArray, bytes: ByteArray): Long {
            published.add("mailbox@${bytes[0].toInt()}")
            return nextId++
        }
    }

    private class FakeOutbox(var owed: SweepDurableEntry? = null) : SweepOutboxPort {
        val recorded = mutableListOf<SweepDurableEntry>()
        val acked = mutableListOf<String>()
        val discarded = mutableListOf<String>()
        var recordThrows: Throwable? = null

        override fun newEntryId(): String = "entry-1"

        override suspend fun record(entry: SweepDurableEntry) {
            recordThrows?.let { throw it }
            recorded.add(entry)
        }

        override suspend fun findOwed(): SweepDurableEntry? = owed

        override suspend fun ack(id: String, commitMessageId: Long, welcomeMessageId: Long?) {
            acked.add(id)
        }

        override suspend fun discard(id: String) {
            discarded.add(id)
        }
    }

    private class FakeCursor(var value: Long = 0) : SweepCursorRef {
        override fun read(): Long = value

        override fun advance(to: Long) {
            value = to
        }
    }

    private fun ports(
        group: FakeGroup = FakeGroup(),
        journal: FakeJournal = FakeJournal(),
        outbox: FakeOutbox = FakeOutbox(),
        cursor: FakeCursor = FakeCursor(),
        acting: ActingCommitter = ActingCommitter.Act,
        persisted: MutableList<Unit> = mutableListOf(),
    ) = SweepPorts(
        group = group,
        journal = journal,
        outbox = outbox,
        cursor = cursor,
        actingCommitter = { acting },
        welcomeMailbox = { key -> key },
        wrapWelcome = { welcome -> welcome },
        markOwnPublished = {},
        persist = { persisted.add(Unit) },
        encodeB64 = { bytes -> bytes.joinToString(",") { it.toString() } },
        decodeB64 = { s ->
            if (s.isEmpty()) ByteArray(0) else s.split(",").map { it.toByte() }.toByteArray()
        },
    )

    // ---------------------------------------------------------------- the five

    /** Obligation 5 + the happy path: the owner sweeps a pending proposal and
     *  the Welcome reaches the added member's mailbox. */
    @Test
    fun `a proposal reaches the committer and is swept`() = runTest {
        val group = FakeGroup(rank = 0u, pending = 1)
        val journal = FakeJournal()
        val outbox = FakeOutbox()
        val outcome = runCommitterSweep(ports(group, journal, outbox))

        assertTrue("outcome was $outcome", outcome is SweepOutcome.Committed)
        assertEquals(1, group.merged)
        assertEquals(0, group.abandoned)
        assertFalse("no sweep may be left staged", group.hasStagedSweep())
        assertEquals(listOf("commit@1", "mailbox@2"), journal.published)
        assertEquals(listOf("entry-1"), outbox.acked)
        assertEquals(1, (outcome as SweepOutcome.Committed).addedCount)
    }

    /** Obligation 4: the durable record lands BEFORE anything reaches the wire. */
    @Test
    fun `the durable record lands before any publish`() = runTest {
        val journal = FakeJournal()
        val outbox = FakeOutbox()
        val order = mutableListOf<String>()
        val recordingOutbox = object : SweepOutboxPort by outbox {
            override suspend fun record(entry: SweepDurableEntry) {
                order.add("record")
                outbox.record(entry)
            }
        }
        val recordingJournal = object : SweepJournalPort by journal {
            override suspend fun publishGroup(bytes: ByteArray): Long {
                order.add("publish")
                return journal.publishGroup(bytes)
            }
        }
        runCommitterSweep(
            SweepPorts(
                group = FakeGroup(),
                journal = recordingJournal,
                outbox = recordingOutbox,
                cursor = FakeCursor(),
                actingCommitter = { ActingCommitter.Act },
                welcomeMailbox = { it },
                wrapWelcome = { it },
                markOwnPublished = {},
                persist = {},
                encodeB64 = { b -> b.joinToString(",") { it.toString() } },
                decodeB64 = { s -> s.split(",").map { it.toByte() }.toByteArray() },
            ),
        )
        assertEquals(listOf("record", "publish"), order)
    }

    /** Obligation 4, fail-closed half: a failed durable write ABORTS with nothing
     *  on the wire, and the staged sweep is rolled back per-group. */
    @Test
    fun `a failed durable record aborts before any publish`() = runTest {
        val group = FakeGroup()
        val journal = FakeJournal()
        val outbox = FakeOutbox().apply { recordThrows = IllegalStateException("disk full") }

        assertThrows(IllegalStateException::class.java) {
            runBlocking { runCommitterSweep(ports(group, journal, outbox)) }
        }
        assertEquals(emptyList<String>(), journal.published)
        assertEquals("the staged sweep must be rolled back", 1, group.abandoned)
        assertFalse(group.hasStagedSweep())
    }

    /** Obligation 1 + the loss path: a foreign commit ordered below ours means we
     *  LOST, so we abandon, publish no Welcome, and the proposals survive. */
    @Test
    fun `a losing sweep abandons and the proposals survive`() = runTest {
        val group = FakeGroup(rank = 0u, pending = 1)
        group.foreignCommits.add(42)
        val journal = FakeJournal(
            // Our commit takes DS id 9, so the winner at id 3 is BELOW it.
            rows = listOf(SweepJournalRow(id = 3, payload = byteArrayOf(42))),
            // The obligation-3 stand-down read must be empty, or the sweep
            // correctly refuses to stage and never reaches the verdict.
            scripted = mutableListOf(emptyList()),
        )
        journal.nextId = 9
        val outbox = FakeOutbox()

        val outcome = runCommitterSweep(ports(group, journal, outbox))

        assertTrue("outcome was $outcome", outcome is SweepOutcome.Lost)
        assertEquals(1, group.abandoned)
        assertEquals(0, group.merged)
        assertFalse("no sweep may be left staged", group.hasStagedSweep())
        assertEquals("the proposals must survive for the winner", 1, group.pending)
        assertEquals(listOf("commit@1"), journal.published)
        assertEquals(listOf("entry-1"), outbox.discarded)
        assertEquals(emptyList<String>(), outbox.acked)
    }

    /** Obligation 2, stated as its own assertion: the mailbox fan-out happens
     *  only on the merge branch. */
    @Test
    fun `welcome is not published before the verdict`() = runTest {
        val group = FakeGroup(rank = 0u, pending = 1)
        group.foreignCommits.add(42)
        val journal = FakeJournal(
            rows = listOf(SweepJournalRow(id = 3, payload = byteArrayOf(42))),
            scripted = mutableListOf(emptyList()),
        )
        journal.nextId = 9

        runCommitterSweep(ports(group, journal))

        assertEquals(
            "a joiner must never open into a state the group does not hold",
            emptyList<String>(),
            journal.published.filter { it.startsWith("mailbox@") },
        )
    }

    /** Obligation 1, restart half. */
    @Test
    fun `a restart finishes an owed verdict`() = runTest {
        val group = FakeGroup(rank = 0u, pending = 1).apply { hasStaged = true }
        val journal = FakeJournal()
        val outbox = FakeOutbox(
            owed = SweepDurableEntry(
                id = "entry-restart",
                commitB64 = "1",
                welcomeEnvelopeB64 = "2",
                welcomeMailboxesB64 = listOf("9"),
            ),
        )

        val outcome = resolveOwedVerdict(ports(group, journal, outbox))

        assertTrue("outcome was $outcome", outcome is SweepOutcome.Committed)
        assertEquals(1, group.merged)
        assertFalse("the freeze must be lifted", group.hasStagedSweep())
        assertEquals(listOf("commit@1", "mailbox@2"), journal.published)
        assertEquals(listOf("entry-restart"), outbox.acked)
    }

    /**
     * Obligation 1, and the reason the staged check runs AHEAD of the rank check:
     * a device demoted while holding a staged sweep must still reach a verdict,
     * or its membership operations are frozen forever with nothing that could
     * ever unwedge them.
     */
    @Test
    fun `an owed verdict is resolved even after a demotion`() = runTest {
        val group = FakeGroup(rank = null, pending = 1).apply { hasStaged = true }
        group.foreignCommits.add(42)
        val journal = FakeJournal(rows = listOf(SweepJournalRow(id = 3, payload = byteArrayOf(42))))
        journal.nextId = 9
        val outbox = FakeOutbox(
            owed = SweepDurableEntry("entry-demoted", "1", null, emptyList()),
        )

        val outcome = runCommitterSweep(ports(group, journal, outbox))

        assertTrue("outcome was $outcome", outcome is SweepOutcome.Lost)
        assertFalse("the demoted device must be unwedged", group.hasStagedSweep())
    }

    /** A staged sweep with no durable record: the commit provably never reached
     *  the wire, so abandoning is correct AND is the unwedge. */
    @Test
    fun `a staged sweep with no record is abandoned as an orphan`() = runTest {
        val group = FakeGroup().apply { hasStaged = true }
        val journal = FakeJournal()

        val outcome = resolveOwedVerdict(ports(group, journal, FakeOutbox(owed = null)))

        assertEquals(SweepOutcome.AbandonedOrphan, outcome)
        assertEquals(1, group.abandoned)
        assertFalse(group.hasStagedSweep())
        assertEquals(emptyList<String>(), journal.published)
    }

    /** Obligation 3. */
    @Test
    fun `an unapplied row stands the sweep down`() = runTest {
        val group = FakeGroup()
        val journal = FakeJournal(
            scripted = mutableListOf(listOf(SweepJournalRow(id = 1, payload = byteArrayOf(7)))),
        )

        val outcome = runCommitterSweep(ports(group, journal))

        assertTrue("outcome was $outcome", outcome is SweepOutcome.StoodDown)
        assertEquals(1, (outcome as SweepOutcome.StoodDown).unappliedRows)
        assertFalse("nothing may be staged on a stand-down", group.hasStagedSweep())
        assertEquals(emptyList<String>(), journal.published)
    }

    /** Obligation 5: a group with NO role table keeps direct commits — nothing is
     *  ever swept on it, and the short-circuit happens before any network. */
    @Test
    fun `a group with no role table is never swept`() = runTest {
        // `ownCommitterRank` is `null` exactly when there is no role table (or
        // this device is a plain member). `null` and `0u` are NOT
        // interchangeable: `0u` is the OWNER.
        val group = FakeGroup(rank = null, pending = 3)
        val journal = FakeJournal()

        val outcome = runCommitterSweep(ports(group, journal))

        assertTrue("outcome was $outcome", outcome is SweepOutcome.NotCommitter)
        assertEquals("a legacy group must not touch the network", 0, journal.fetches)
        assertFalse(group.hasStagedSweep())
    }

    /** S4b: a candidate stands down for someone strictly better placed. */
    @Test
    fun `a candidate defers to a fresher lower rank`() = runTest {
        val group = FakeGroup(rank = 3u, pending = 1)
        val journal = FakeJournal()

        val outcome = runCommitterSweep(
            ports(group, journal, acting = ActingCommitter.Defer(1u)),
        )

        assertTrue("outcome was $outcome", outcome is SweepOutcome.Deferred)
        assertEquals(3u, (outcome as SweepOutcome.Deferred).rank)
        assertEquals(1u, outcome.toRank)
        assertEquals("a deferral must not touch the network", 0, journal.fetches)
        assertFalse(group.hasStagedSweep())
    }

    /** S4b, fail-closed: an unreadable view of who else is running is a DEFER,
     *  never a licence to sweep on no evidence. */
    @Test
    fun `an unreadable acting verdict defers`() = runTest {
        val group = FakeGroup(rank = 2u, pending = 1)
        val journal = FakeJournal()

        val outcome = runCommitterSweep(
            SweepPorts(
                group = group,
                journal = journal,
                outbox = FakeOutbox(),
                cursor = FakeCursor(),
                actingCommitter = { throw IllegalStateException("store unreadable") },
                welcomeMailbox = { it },
                wrapWelcome = { it },
                markOwnPublished = {},
                persist = {},
                encodeB64 = { b -> b.joinToString(",") { it.toString() } },
                decodeB64 = { s -> s.split(",").map { it.toByte() }.toByteArray() },
            ),
        )

        assertTrue("outcome was $outcome", outcome is SweepOutcome.Deferred)
        assertFalse(group.hasStagedSweep())
    }

    /** An unreadable journal leaves the sweep STAGED on purpose — both
     *  alternatives (merge on an unread verdict / discard a commit already on the
     *  wire) are worse than a self-healing freeze. */
    @Test
    fun `an unreadable verdict leaves the sweep staged`() = runTest {
        val group = FakeGroup()
        val journal = object : SweepJournalPort {
            var calls = 0
            override suspend fun fetch(after: Long): List<SweepJournalRow> {
                calls++
                if (calls == 1) return emptyList()
                throw IllegalStateException("DS unreachable")
            }

            override suspend fun publishGroup(bytes: ByteArray): Long = 1

            override suspend fun publishMailbox(pseudonym: ByteArray, bytes: ByteArray): Long = 2
        }

        assertThrows(SweepVerdictUnavailable::class.java) {
            runBlocking {
                runCommitterSweep(
                SweepPorts(
                    group = group,
                    journal = journal,
                    outbox = FakeOutbox(),
                    cursor = FakeCursor(),
                    actingCommitter = { ActingCommitter.Act },
                    welcomeMailbox = { it },
                    wrapWelcome = { it },
                    markOwnPublished = {},
                    persist = {},
                    encodeB64 = { b -> b.joinToString(",") { it.toString() } },
                        decodeB64 = { s -> s.split(",").map { it.toByte() }.toByteArray() },
                    ),
                )
            }
        }
        assertTrue("the sweep must stay staged for the retry", group.hasStagedSweep())
    }

    /**
     * The Android-specific obligation: the adjudication region — both epoch
     * reads, the whole apply loop and the cursor advance — is ONE provider-lock
     * hold.
     *
     * Between two applied rows this group is applied-but-not-persisted, and a
     * sibling group's whole-provider `restoreStorage` (the ADR-0040 F1 abort
     * path) observing that state would roll the applied rows back while the
     * cursor had already moved past them.
     */
    @Test
    fun `the adjudication apply is one provider region`() = runTest {
        val group = FakeGroup(rank = 0u, pending = 1)
        group.foreignCommits.add(42)
        val journal = FakeJournal(
            rows = listOf(
                SweepJournalRow(id = 2, payload = byteArrayOf(7)),
                SweepJournalRow(id = 3, payload = byteArrayOf(42)),
                SweepJournalRow(id = 4, payload = byteArrayOf(8)),
            ),
            scripted = mutableListOf(emptyList()),
        )
        journal.nextId = 9
        val cursor = FakeCursor()

        runCommitterSweep(ports(group, journal, cursor = cursor))

        assertEquals(listOf(7, 42, 8), group.applied)
        assertEquals("the cursor advances over exactly the rows applied", 4L, cursor.value)
        // Every apply must fall inside ONE provider region — not one region per
        // row. Between two rows this group is applied-but-not-persisted, and a
        // sibling group's whole-provider `restoreStorage` observing that state
        // would roll the applied rows back while the cursor had moved past them.
        //
        // The trace is asserted WHOLE rather than by a substring, so the shape of
        // every other region (the prelude read, the stage, the merge) is pinned
        // too and a region that quietly split cannot hide behind a match.
        assertEquals(
            "the whole apply loop must be ONE provider region",
            listOf(
                // prelude: hasStagedSweep + rank + pending count
                "enter", "exit",
                // stage
                "enter", "exit",
                // adjudicate: epochBefore, all three applies, cursor, epochAfter
                "enter", "apply", "apply", "apply", "exit",
                // abandon (a foreign commit was ordered ahead of ours)
                "enter", "exit",
            ),
            group.providerTrace.toList(),
        )
    }

    /** The cursor never advances past our own commit — the poll re-fetches it and
     *  drops it through the echo filter. */
    @Test
    fun `the cursor never advances past our own commit`() = runTest {
        val group = FakeGroup(rank = 0u, pending = 1)
        val journal = FakeJournal(
            rows = listOf(
                SweepJournalRow(id = 1, payload = byteArrayOf(7)),
                SweepJournalRow(id = 5, payload = byteArrayOf(8)),
            ),
            scripted = mutableListOf(emptyList()),
        )
        val cursor = FakeCursor()

        runCommitterSweep(ports(group, journal, cursor = cursor))

        // Our commit took DS id 1, so nothing below it exists and nothing is
        // applied; the cursor must not move at all.
        assertEquals(emptyList<Int>(), group.applied)
        assertEquals(0L, cursor.value)
    }

    /** The pure rules, pinned directly. */
    @Test
    fun `the pure verdict and stand-down rules`() {
        assertTrue("an unmoved epoch means we won", sweepVerdict(7uL, 7uL))
        assertFalse("a moved epoch means a foreign commit won", sweepVerdict(7uL, 8uL))
        assertFalse(shouldStandDown(0))
        assertTrue(shouldStandDown(1))
    }
}
