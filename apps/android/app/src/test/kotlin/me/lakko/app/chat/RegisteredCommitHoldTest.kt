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

    @Test
    fun `hold - a commit framed at the local epoch with the next epoch registered`() {
        assertEquals(RegisteredCommitVerdict.HOLD, registeredCommitVerdict(false, 6uL, 7uL, 6uL, 7uL))
        assertEquals(RegisteredCommitVerdict.HOLD, registeredCommitVerdict(false, 6uL, 7uL, 6uL, 16uL))
    }

    @Test
    fun `skip - every other case`() {
        val skip = RegisteredCommitVerdict.SKIP
        // own echo
        assertEquals(skip, registeredCommitVerdict(true, 6uL, 7uL, 6uL, 7uL))
        // unknown epochs
        assertEquals(skip, registeredCommitVerdict(false, null, 7uL, 6uL, 7uL))
        assertEquals(skip, registeredCommitVerdict(false, 6uL, null, 6uL, 7uL))
        assertEquals(skip, registeredCommitVerdict(false, 6uL, 7uL, null, 7uL))
        // not a commit (an application message or a proposal)
        assertEquals(skip, registeredCommitVerdict(false, 6uL, 6uL, 6uL, 7uL))
        // sealed below local (a commit this device is past)
        assertEquals(skip, registeredCommitVerdict(false, 5uL, 6uL, 6uL, 7uL))
        // sealed above local (a commit of an epoch beyond the next)
        assertEquals(skip, registeredCommitVerdict(false, 7uL, 8uL, 6uL, 8uL))
        // latest unknown (unclaimed journal, or no latest_epoch on the read)
        assertEquals(skip, registeredCommitVerdict(false, 6uL, 7uL, 6uL, null))
        // latest == local: the DS registered no later epoch
        assertEquals(skip, registeredCommitVerdict(false, 6uL, 7uL, 6uL, 6uL))
        // no next epoch exists
        assertEquals(
            skip,
            registeredCommitVerdict(false, ULong.MAX_VALUE, 0uL, ULong.MAX_VALUE, ULong.MAX_VALUE),
        )
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
            val decision = holds.recordFailure(GROUP, ROW, 6uL, 6uL)
            assertEquals(HoldDecision(attempt = index + 1, held = true), decision)
            assertEquals(delaySecs * 1_000L, holds.msUntilDue(GROUP))
            assertFalse(holds.isDue(GROUP))
            clock.now += delaySecs * 1_000L - 1
            assertFalse("not due one ms early", holds.isDue(GROUP))
            clock.now += 1
            assertTrue(holds.isDue(GROUP))
            assertEquals(0L, holds.msUntilDue(GROUP))
        }
        val last = holds.recordFailure(GROUP, ROW, 6uL, 6uL)
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
        holds.recordFailure(GROUP, ROW, 6uL, 6uL)
        holds.recordFailure(GROUP, ROW, 6uL, 6uL)
        assertEquals(2, holds.hold(GROUP)?.attempts)
        assertEquals(1, holds.recordFailure(GROUP, ROW + 1, 6uL, 6uL).attempt)
        holds.release(GROUP, ROW)
        assertNotNull("another row's release does not drop it", holds.hold(GROUP))
        holds.release(GROUP, ROW + 1)
        assertNull(holds.hold(GROUP))
        assertTrue(holds.isDue(GROUP))
    }

    @Test
    fun `the behind mark goes once the epoch moves above it, and blocks repair until then`() {
        val holds = RegisteredCommitHoldTracker { 0L }
        repeat(REGISTERED_COMMIT_MAX_ATTEMPTS) { holds.recordFailure(GROUP, ROW, 6uL, 6uL) }
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
        holds.recordFailure(GROUP.uppercase(), ROW, 6uL, 6uL)
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
        MailboxMessage(id = id, payload = byteArrayOf(sealed.toByte(), after.toByte()), createdAt = "")

    private fun apply(
        group: FakeGroup,
        rows: List<MailboxMessage>,
        after: Long,
        latest: ULong?,
        holds: RegisteredCommitHoldTracker,
        lines: MutableList<String> = mutableListOf(),
        groupIdHex: String = GROUP,
    ): JournalRowsApplied {
        val byPayload = rows.associateBy { it.payload.toList() }
        return applyJournalRowsHolding(
            groupIdHex = groupIdHex,
            rows = rows,
            after = after,
            latestEpoch = latest,
            holds = holds,
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
