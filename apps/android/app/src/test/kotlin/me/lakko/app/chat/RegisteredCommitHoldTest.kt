// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.ds.MailboxMessage
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiException

/**
 * ADR-0091 phase 1 — the conversation poll does not skip the commit the DS
 * registered as this device's next epoch.
 *
 * The verdict and the tracker are pure; the apply loop is driven through
 * [applyJournalRowsHolding], the seam `ChatClient.applyFetchedMessages` calls,
 * with a fake group whose rows say what they are. `applyFetchedMessages`
 * itself takes the FFI `Group` and is not run here (the host JVM does not load
 * the native core), and neither is the poll gate in
 * `ChatRepository.pollGroupOnMlsThread`; the gate's arithmetic is
 * [RegisteredCommitHoldTracker.msUntilDue], tested below.
 */
class RegisteredCommitHoldTest {

    // -----------------------------------------------------------------------
    // 1. The verdict truth table
    // -----------------------------------------------------------------------

    /** Row 7967 is the commit the DS registered as leaving epoch 6. */
    private val registered: Map<ULong, Long> = mapOf(7uL to 7967L, 8uL to 8100L)

    @Test
    fun `hold - the commit the DS registered as leaving this device's epoch`() {
        assertEquals(RegisteredCommitVerdict.HOLD, registeredCommitVerdict(false, 7967L, 6uL, registered))
        assertEquals(RegisteredCommitVerdict.HOLD, registeredCommitVerdict(false, 8100L, 7uL, registered))
    }

    @Test
    fun `skip - every other case`() {
        val skip = RegisteredCommitVerdict.SKIP
        // an own echo
        assertEquals(skip, registeredCommitVerdict(true, 7967L, 6uL, registered))
        // no local epoch
        assertEquals(skip, registeredCommitVerdict(false, 7967L, null, registered))
        // a read that named no commits, or an empty list
        assertEquals(skip, registeredCommitVerdict(false, 7967L, 6uL, null))
        assertEquals(skip, registeredCommitVerdict(false, 7967L, 6uL, emptyMap()))
        // another row at the same epoch (a commit-shaped row through the plain
        // publish route)
        assertEquals(skip, registeredCommitVerdict(false, 7966L, 6uL, registered))
        // the commit of the epoch after the next
        assertEquals(skip, registeredCommitVerdict(false, 8100L, 6uL, registered))
        // a commit this device is already past
        assertEquals(skip, registeredCommitVerdict(false, 7967L, 7uL, registered))
        // no next epoch exists
        assertEquals(skip, registeredCommitVerdict(false, 7967L, ULong.MAX_VALUE, registered))
    }

    @Test
    fun `names the commit leaving an epoch`() {
        assertEquals(7967L, registeredCommitLeaving(registered, 6uL))
        assertNull(registeredCommitLeaving(registered, 8uL))
        assertNull(registeredCommitLeaving(null, 6uL))
        assertNull(registeredCommitLeaving(registered, null))
    }

    // -----------------------------------------------------------------------
    // 2. The tracker
    // -----------------------------------------------------------------------

    private class Clock(var now: Long = 1_000_000L) {
        fun read(): Long = now
    }

    @Test
    fun `backoff follows the schedule and exhausts at the eighth attempt`() {
        val clock = Clock()
        val holds = RegisteredCommitHoldTracker(clock::read)
        val expected = listOf(2L, 5L, 15L, 30L, 60L, 120L, 300L)
        for ((index, delaySecs) in expected.withIndex()) {
            val decision = holds.recordFailure(GROUP, ROW, 6uL)
            assertEquals(HoldDecision(attempt = index + 1, held = true), decision)
            assertEquals(delaySecs * 1_000L, holds.msUntilDue(GROUP))
            assertFalse(holds.isDue(GROUP))
            clock.now += delaySecs * 1_000L - 1
            assertFalse("not due one ms early", holds.isDue(GROUP))
            clock.now += 1
            assertTrue(holds.isDue(GROUP))
            assertEquals(0L, holds.msUntilDue(GROUP))
        }
        val last = holds.recordFailure(GROUP, ROW, 6uL)
        assertEquals(HoldDecision(attempt = REGISTERED_COMMIT_MAX_ATTEMPTS, held = false), last)
        assertNull("the hold is dropped", holds.hold(GROUP))
        assertEquals(BehindMark(epoch = 6uL, atMs = clock.now), holds.behindMark(GROUP))
        assertTrue(holds.isDue(GROUP))
        assertNull(holds.msUntilDue(GROUP))
    }

    @Test
    fun `a new row restarts the count, an applied row clears the hold`() {
        val clock = Clock()
        val holds = RegisteredCommitHoldTracker(clock::read)
        holds.recordFailure(GROUP, ROW, 6uL)
        holds.recordFailure(GROUP, ROW, 6uL)
        assertEquals(2, holds.hold(GROUP)?.attempts)
        assertEquals(1, holds.recordFailure(GROUP, ROW + 1, 6uL).attempt)
        holds.release(GROUP, ROW)
        assertNotNull("another row's release does not drop it", holds.hold(GROUP))
        holds.release(GROUP, ROW + 1)
        assertNull(holds.hold(GROUP))
        assertTrue(holds.isDue(GROUP))
    }

    @Test
    fun `the behind mark goes once the epoch moves above it, and blocks repair until then`() {
        val holds = RegisteredCommitHoldTracker { 0L }
        repeat(REGISTERED_COMMIT_MAX_ATTEMPTS) { holds.recordFailure(GROUP, ROW, 6uL) }
        assertTrue(holds.repairBlocked(GROUP, 6uL))
        assertTrue(holds.repairBlocked(GROUP, 5uL))
        assertFalse(holds.repairBlocked(GROUP, 7uL))
        holds.observeEpoch(GROUP, 6uL)
        assertNotNull(holds.behindMark(GROUP))
        holds.observeEpoch(GROUP, 7uL)
        assertNull(holds.behindMark(GROUP))
        assertFalse(holds.repairBlocked(GROUP, 6uL))
    }

    @Test
    fun `groups are independent and the key is case-insensitive`() {
        val holds = RegisteredCommitHoldTracker { 0L }
        holds.recordFailure(GROUP.uppercase(), ROW, 6uL)
        assertNotNull(holds.hold(GROUP))
        assertNull(holds.hold(OTHER_GROUP))
        assertTrue(holds.repairBlocked(GROUP, 6uL))
        assertFalse(holds.repairBlocked(OTHER_GROUP, 6uL))
    }

    // -----------------------------------------------------------------------
    // 3. The apply loop
    // -----------------------------------------------------------------------

    /**
     * A fake group: each row's payload is `[sealed, after]`. A row applies when
     * it is framed at the current epoch (a commit moves the epoch to `after`);
     * a commit listed in [failing] fails that many more times first.
     */
    private class FakeGroup(var epoch: ULong) {
        val failing = mutableMapOf<Long, Int>()
        val applied = mutableListOf<Long>()

        fun process(row: MailboxMessage) {
            val sealed = row.payload[0].toULong()
            val after = row.payload[1].toULong()
            val left = failing[row.id] ?: 0
            if (left > 0) {
                failing[row.id] = left - 1
                throw FfiException.Group("InvalidCommit(MissingProposal)")
            }
            if (sealed != epoch) throw FfiException.Group("WrongEpoch")
            applied += row.id
            if (after > sealed) epoch = after
        }
    }

    private fun row(id: Long, sealed: Int, after: Int) =
        MailboxMessage(id = id, payload = byteArrayOf(sealed.toByte(), after.toByte(), id.toByte(), (id shr 8).toByte()), createdAt = "")

    /** What the DS would name for [rows]: the first commit framed at each
     *  epoch up to [latest] is registered (none without [latest]). */
    private fun registeredOf(rows: List<MailboxMessage>, latest: ULong?): Map<ULong, Long>? {
        if (latest == null) return null
        val out = mutableMapOf<ULong, Long>()
        for (r in rows) {
            val sealed = r.payload[0].toULong()
            val after = r.payload[1].toULong()
            if (after > sealed && after <= latest && !out.containsKey(after)) out[after] = r.id
        }
        return out
    }

    private fun apply(
        group: FakeGroup,
        rows: List<MailboxMessage>,
        after: Long,
        latest: ULong?,
        holds: RegisteredCommitHoldTracker,
        lines: MutableList<String> = mutableListOf(),
        groupIdHex: String = GROUP,
        commits: Map<ULong, Long>? = registeredOf(rows, latest),
        readFrom: Long = after,
    ): JournalRowsApplied {
        val byPayload = rows.associateBy { it.payload.toList() }
        return applyJournalRowsHolding(
            groupIdHex = groupIdHex,
            rows = rows,
            after = after,
            latestEpoch = latest,
            registeredCommits = commits,
            holds = holds,
            readFrom = readFrom,
            isOwnEchoRow = { false },
            localEpoch = { group.epoch },
            process = { payload -> group.process(byPayload.getValue(payload.toList())) },
            framedEpochs = { payload -> payload[0].toULong() to payload[1].toULong() },
            isOwnEchoFailure = { false },
            log = { _, line -> lines += line },
        ) { _, _ -> }
    }

    @Test
    fun `a registered commit that fails is held, not skipped, and applies on the next due poll`() {
        val clock = Clock()
        val holds = RegisteredCommitHoldTracker(clock::read)
        val group = FakeGroup(epoch = 6uL)
        group.failing[11L] = 1
        // 10: an application row at 6; 11: the commit 6 -> 7; 12: a row at 7.
        val rows = listOf(row(10L, 6, 6), row(11L, 6, 7), row(12L, 7, 7))

        val first = apply(group, rows, after = 9L, latest = 7uL, holds = holds)
        assertTrue(first.held)
        assertEquals("the cursor stays before the commit", 10L, first.cursor)
        assertEquals("the row after it is not consumed", listOf(10L), group.applied)
        assertEquals(6uL, group.epoch)
        assertFalse("not due before the backoff", holds.isDue(GROUP))

        clock.now += 2_000L
        assertTrue(holds.isDue(GROUP))
        // The next poll reads from the cursor it stored.
        val second = apply(group, rows.filter { it.id > first.cursor }, after = first.cursor, latest = 7uL, holds = holds)
        assertFalse(second.held)
        assertEquals(12L, second.cursor)
        assertEquals(listOf(10L, 11L, 12L), group.applied)
        assertEquals(7uL, group.epoch)
        assertNull("the hold is gone", holds.hold(GROUP))
    }

    @Test
    fun `a failing row that is not the registered commit is still skipped`() {
        val holds = RegisteredCommitHoldTracker { 0L }
        val group = FakeGroup(epoch = 6uL)
        // 20: a commit at 6 while the DS names no later epoch; 21: an
        // application row of an older epoch; 22: an application row at 6.
        group.failing[20L] = 1
        val rows = listOf(row(20L, 6, 7), row(21L, 5, 5), row(22L, 6, 6))
        val result = apply(group, rows, after = 19L, latest = 6uL, holds = holds)
        assertFalse(result.held)
        assertEquals("the cursor moves past both", 22L, result.cursor)
        assertEquals(listOf(22L), group.applied)
        assertNull(holds.hold(GROUP))
    }

    @Test
    fun `eight failures skip the commit and leave a behind mark`() {
        val clock = Clock()
        val holds = RegisteredCommitHoldTracker(clock::read)
        val group = FakeGroup(epoch = 6uL)
        group.failing[11L] = 100
        val rows = listOf(row(11L, 6, 7), row(12L, 7, 7))
        val lines = mutableListOf<String>()
        var cursor = 10L
        repeat(REGISTERED_COMMIT_MAX_ATTEMPTS - 1) {
            val r = apply(group, rows, after = cursor, latest = 7uL, holds = holds, lines = lines)
            assertTrue(r.held)
            cursor = r.cursor
            clock.now += 1_000_000L
        }
        assertEquals(10L, cursor)
        val last = apply(group, rows, after = cursor, latest = 7uL, holds = holds, lines = lines)
        assertFalse(last.held)
        assertEquals("skipped as before", 12L, last.cursor)
        assertNull(holds.hold(GROUP))
        assertEquals(6uL, holds.behindMark(GROUP)?.epoch)
        assertTrue(
            lines.any {
                it == "chat registered commit skipped after 8 attempts: " +
                    "this device is now behind in a conversation (local=6 latest=7)"
            },
        )
        assertTrue(lines.any { it.endsWith("verdict=hold attempt=8/8") })
    }

    @Test
    fun `a commit refused by this device's rules is held and logged as a divergence`() {
        val holds = RegisteredCommitHoldTracker { 0L }
        val lines = mutableListOf<String>()
        val result = applyJournalRowsHolding(
            groupIdHex = GROUP,
            rows = listOf(row(11L, 6, 7)),
            after = 10L,
            latestEpoch = 7uL,
            registeredCommits = mapOf(7uL to 11L),
            holds = holds,
            isOwnEchoRow = { false },
            localEpoch = { 6uL },
            process = { throw FfiException.MembershipNotOwner("x") },
            framedEpochs = { 6uL to 7uL },
            isOwnEchoFailure = { false },
            log = { level, line -> lines += "$level $line" },
        ) { _, _: Unit -> }
        assertTrue(result.held)
        assertEquals(
            listOf(
                "WARN chat row not applied: kind=refused:notOwner sealed=6 after=7 local=6 " +
                    "latest=7 verdict=hold attempt=1/8",
                "ERROR chat registered commit refused by this device's rules (refused:notOwner): " +
                    "the other devices applied it; a bug or version skew",
            ),
            lines,
        )
    }

    @Test
    fun `an own echo is skipped, never held`() {
        val holds = RegisteredCommitHoldTracker { 0L }
        val result = applyJournalRowsHolding(
            groupIdHex = GROUP,
            rows = listOf(row(11L, 6, 7)),
            after = 10L,
            latestEpoch = 7uL,
            registeredCommits = mapOf(7uL to 11L),
            holds = holds,
            isOwnEchoRow = { false },
            localEpoch = { 6uL },
            process = { throw FfiException.Group("CannotDecryptOwnMessage") },
            framedEpochs = { 6uL to 7uL },
            isOwnEchoFailure = { true },
            log = { _, _ -> },
        ) { _, _: Unit -> }
        assertFalse(result.held)
        assertEquals(11L, result.cursor)
    }

    @Test
    fun `a commit-shaped row the DS did not register is skipped at once, the registered one held`() {
        val holds = RegisteredCommitHoldTracker { 0L }
        val group = FakeGroup(epoch = 6uL)
        group.failing[30L] = 100
        group.failing[31L] = 1
        // 30: garbage framed like the commit 6 -> 7; 31: the registered commit.
        val rows = listOf(row(30L, 6, 7), row(31L, 6, 7))
        val result = apply(group, rows, after = 29L, latest = 7uL, holds = holds, commits = mapOf(7uL to 31L))
        assertTrue(result.held)
        assertEquals("past the garbage, before the registered commit", 30L, result.cursor)
        assertEquals(31L, holds.hold(GROUP)?.rowId)
    }

    @Test
    fun `a rewound read starts below the cursor and never returns a cursor below it`() {
        val holds = RegisteredCommitHoldTracker { 0L }
        val group = FakeGroup(epoch = 6uL)
        // The cursor is at 13; the snapshot went back to 6, so the read starts
        // at 10, before the commit 11 that leaves 6.
        val rows = listOf(row(11L, 6, 7), row(12L, 7, 7), row(13L, 7, 7))
        val result = apply(group, rows, after = 13L, latest = 7uL, holds = holds, readFrom = 10L)
        assertFalse(result.held)
        assertEquals(13L, result.cursor)
        assertEquals(listOf(11L, 12L, 13L), group.applied)
        assertEquals(7uL, group.epoch)
        // Held on the rewound commit: still never below the cursor, and the
        // hold survives the next rewound read (the count goes on).
        val stuck = FakeGroup(epoch = 6uL)
        stuck.failing[11L] = 100
        val held = apply(stuck, rows, after = 13L, latest = 7uL, holds = holds, readFrom = 10L)
        assertTrue(held.held)
        assertEquals(13L, held.cursor)
        apply(stuck, rows, after = 13L, latest = 7uL, holds = holds, readFrom = 10L)
        assertEquals(2, holds.hold(GROUP)?.attempts)
    }

    // -----------------------------------------------------------------------
    // 4. No identifier in a log line
    // -----------------------------------------------------------------------

    @Test
    fun `the log lines carry no group id and no row id`() {
        val holds = RegisteredCommitHoldTracker { 0L }
        val group = FakeGroup(epoch = 6uL)
        val rowId = 796_742L
        group.failing[rowId] = 100
        val lines = mutableListOf<String>()
        val rows = listOf(row(rowId - 1, 3, 3), row(rowId, 6, 7))
        repeat(REGISTERED_COMMIT_MAX_ATTEMPTS) {
            apply(group, rows, after = rowId - 2, latest = 7uL, holds = holds, lines = lines, groupIdHex = GROUP)
        }
        assertTrue(lines.size > REGISTERED_COMMIT_MAX_ATTEMPTS)
        for (line in lines) {
            assertFalse(line, line.contains(GROUP, ignoreCase = true))
            assertFalse(line, line.contains(GROUP.take(8), ignoreCase = true))
            assertFalse(line, line.contains(rowId.toString()))
            assertFalse(line, line.contains((rowId - 1).toString()))
        }
        assertEquals(
            "chat row not applied: kind=wrong-epoch sealed=3 after=3 local=6 latest=7 verdict=skip attempt=-/8",
            lines.first(),
        )
    }

    @Test
    fun `failure kinds`() {
        assertEquals("wrong-epoch", failedRowKindFromText("Group WrongEpoch"))
        assertEquals("own-message", failedRowKindFromText("CannotDecryptOwnMessage"))
        assertEquals("missing-proposal", failedRowKindFromText("InvalidCommit(MissingProposal)"))
        assertEquals("decrypt", failedRowKindFromText("AeadError"))
        assertEquals("decrypt", failedRowKindFromText("could not decrypt"))
        assertEquals("other", failedRowKindFromText("something"))
        assertEquals("noRoleTable", refusalCode("NO_ROLE_TABLE"))
        assertEquals("refused:notAMember", failedRowKind(FfiException.MembershipNotAMember("x")))
        assertEquals(
            "refused:notOwner",
            failedRowKind(ChatError("wrapped", FfiException.MembershipNotOwner("x"))),
        )
    }

    private companion object {
        const val GROUP: String = "c14ae3b45bdb6a66c14ae3b45bdb6a66c14ae3b45bdb6a66c14ae3b45bdb6a66"
        const val OTHER_GROUP: String = "aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00"
        const val ROW: Long = 7967L
    }
}
