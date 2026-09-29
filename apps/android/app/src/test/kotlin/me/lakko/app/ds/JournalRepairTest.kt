// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0057 second security pass, finding 1 — the repair from above: a noted
 * group is checked only after the delay, Replace goes out only when the group
 * did not move past the noted epoch, a member never repairs, a head poll that
 * failed for anything but a 401 never leads to a Replace, and a 403 stops.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class JournalRepairTest {

    private val group = "ab".repeat(32)
    private val journal = ByteArray(32) { 3 }

    private class Ports(
        var rank: JournalRepairRank = JournalRepairRank.ADMIN,
        var epoch: ULong = 5uL,
    ) : JournalRepairPorts {
        val signer = RecordingStatementSigner("admin")
        val events = mutableListOf<String>()
        var pollError: Throwable? = null
        var moveOnPoll: ULong? = null
        var putError: Throwable? = null

        override suspend fun rank(groupIdHex: String): JournalRepairRank = rank

        override fun holds(groupIdHex: String): Boolean = true

        override suspend fun pollToHead(groupIdHex: String) {
            events += "poll"
            moveOnPoll?.let { epoch = it }
            pollError?.let { throw it }
        }

        override fun currentEpochKey(groupIdHex: String): EpochKey = EpochKey(epoch, ByteArray(32) { epoch.toByte() })

        override suspend fun repairSigner(groupIdHex: String): DsStatementSigning? =
            if (mayRepair(rank)) signer else null

        override suspend fun put(make: () -> DsStatement) {
            events += make().path.substringAfterLast('/')
            putError?.let { throw it }
        }
    }

    @Test
    fun `an admin's note checks after the delay and replaces at its own epoch`() = runTest {
        val ports = Ports()
        val repair = JournalRepair(this, ports)
        assertTrue(repair.note(group, journal, 5uL, RepairReason.APPEND_CONFLICT, JournalRepairRank.ADMIN))
        advanceTimeBy(JOURNAL_REPAIR_DELAY_MS - 1)
        runCurrent()
        assertTrue("nothing before the delay", ports.events.isEmpty())
        advanceTimeBy(2)
        runCurrent()
        assertEquals(listOf("poll", "access"), ports.events)
        assertEquals(listOf("replace@5"), ports.signer.calls)
        assertTrue(ports.signer.lastRevoke.isEmpty())
        assertFalse(repair.isPending(group))
    }

    @Test
    fun `a member's note is dropped and nothing is ever sent`() = runTest {
        val ports = Ports(rank = JournalRepairRank.MEMBER)
        val repair = JournalRepair(this, ports)
        assertFalse(repair.note(group, journal, 5uL, RepairReason.APPEND_CONFLICT, JournalRepairRank.MEMBER))
        // A note without a known rank is scheduled, and the check refuses a
        // member before any request.
        assertTrue(repair.note(group, journal, 5uL, RepairReason.REFUSED_AT_HEAD))
        advanceTimeBy(JOURNAL_REPAIR_DELAY_MS + 1)
        runCurrent()
        assertTrue(ports.events.isEmpty())
        assertTrue(ports.signer.calls.isEmpty())
        assertEquals(RepairOutcome.NOT_PERMITTED, repair.check(group, journal, 5uL))
    }

    @Test
    fun `no replace when a commit moved the group past the noted epoch`() = runTest {
        val ports = Ports().apply { moveOnPoll = 6uL }
        val repair = JournalRepair(this, ports)
        assertEquals(RepairOutcome.MOVED, repair.check(group, journal, 5uL))
        assertEquals(listOf("poll"), ports.events)
        assertTrue(ports.signer.calls.isEmpty())
    }

    @Test
    fun `a head poll refused with 401 still repairs`() = runTest {
        val ports = Ports().apply { pollError = RuntimeException("wrapped", DsHttpError(401, "/messages", "")) }
        assertEquals(RepairOutcome.REPLACED, JournalRepair(this, ports).check(group, journal, 5uL))
        assertEquals(listOf("replace@5"), ports.signer.calls)
    }

    @Test
    fun `a head poll that failed for another reason never replaces`() = runTest {
        val offline = Ports().apply { pollError = java.io.IOException("offline") }
        assertEquals(RepairOutcome.POLL_FAILED, JournalRepair(this, offline).check(group, journal, 5uL))
        val server = Ports().apply { pollError = DsHttpError(503, "/messages", "") }
        assertEquals(RepairOutcome.POLL_FAILED, JournalRepair(this, server).check(group, journal, 5uL))
        assertTrue(offline.signer.calls.isEmpty())
        assertTrue(server.signer.calls.isEmpty())
    }

    @Test
    fun `a 403 means an equal or higher rank registered above - stop`() = runTest {
        val ports = Ports().apply { putError = DsHttpError(403, "/access", "") }
        assertEquals(RepairOutcome.OUTRANKED, JournalRepair(this, ports).check(group, journal, 5uL))
    }

    @Test
    fun `one check per group at a time`() = runTest {
        val ports = Ports(rank = JournalRepairRank.ROOT)
        val repair = JournalRepair(this, ports)
        assertTrue(repair.note(group, journal, 5uL, RepairReason.APPEND_CONFLICT, JournalRepairRank.ROOT))
        assertFalse(repair.note(group, journal, 5uL, RepairReason.REFUSED_AT_HEAD))
        assertTrue(repair.isPending(group))
        advanceTimeBy(JOURNAL_REPAIR_DELAY_MS + 1)
        runCurrent()
        assertEquals(listOf("replace@5"), ports.signer.calls)
        assertFalse(repair.isPending(group))
    }

    @Test
    fun `a demotion between the note and the check stops the replace`() = runTest {
        val ports = Ports()
        val repair = JournalRepair(this, ports)
        repair.note(group, journal, 5uL, RepairReason.APPEND_CONFLICT, JournalRepairRank.ADMIN)
        ports.rank = JournalRepairRank.MEMBER
        advanceTimeBy(JOURNAL_REPAIR_DELAY_MS + 1)
        runCurrent()
        assertTrue(ports.signer.calls.isEmpty())
    }

    @Test
    fun `a status is found anywhere in a wrapped error chain`() {
        assertEquals(401, dsStatusDeep(IllegalStateException("x", RuntimeException("y", DsHttpError(401, "/p", "")))))
        assertEquals(null, dsStatusDeep(java.io.IOException("x")))
    }
}
