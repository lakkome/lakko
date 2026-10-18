// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import java.util.Base64
import kotlinx.coroutines.async
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.yield
import me.lakko.app.ds.DsCommitAnswer
import me.lakko.app.ds.DsCommitConflictError
import me.lakko.app.ds.DsJournalSigner
import me.lakko.app.ds.FakeJournalSigner
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0081 decision 2 — the DeviceGroup commit outbox, driven over its ports.
 * Each failure path of the ADR's Context has a case:
 *
 *   1. a crash between the DS registration and the persist: the entry written
 *      before the send carries the commit, and the restart finishes it;
 *   2. a failed publish after the persist: the entry stays and is retried;
 *   3. a swallowed persist failure: making N+1 current fails closed and the
 *      entry stays.
 *
 * Mirrors `apps/web/src/devices/device-group-commit.test.ts`.
 */
class DeviceGroupCommitTest {

    private class StepFailed(val step: String) : IllegalStateException(step)

    private class Model(start: ULong = 5uL) : DeviceGroupCommitPorts {
        var durable: ULong = start
        var slot: DeviceGroupCommitEntry? = null
        val journal = mutableListOf<Pair<ULong, String>>()
        private val registered = HashMap<ULong, String>()
        val log = mutableListOf<String>()
        val failNext = HashMap<String, Int>()

        private fun step(name: String) {
            log += name
            val n = failNext[name] ?: 0
            if (n > 0) {
                failNext[name] = n - 1
                throw StepFailed(name)
            }
        }

        fun takeEpochElsewhere(epoch: ULong) {
            registered[epoch] = "elsewhere"
            journal += epoch to "elsewhere"
        }

        override fun saveEntry(entry: DeviceGroupCommitEntry) {
            step("save")
            slot = entry
        }

        override fun loadEntry(): DeviceGroupCommitEntry? = slot

        override fun deleteEntry() {
            log += "delete"
            slot = null
        }

        override fun durableProof(): DsJournalSigner = FakeJournalSigner(durable)

        override suspend fun commit(entry: DeviceGroupCommitEntry, proof: DsJournalSigner): DsCommitAnswer {
            step("commit")
            val epoch = entry.epochValue!!
            registered[epoch]?.let { held ->
                if (held == entry.rowB64) {
                    val id = journal.indexOfFirst { it.first == epoch } + 1
                    return DsCommitAnswer(id.toLong(), "t", resent = true)
                }
                throw DsCommitConflictError("/ds/group/ab/commit", "")
            }
            check(proof.epoch + 1uL == epoch) { "bad proof epoch" }
            registered[epoch] = entry.rowB64
            journal += epoch to entry.rowB64
            return DsCommitAnswer(journal.size.toLong(), "t", resent = false)
        }

        override fun makeCurrent(entry: DeviceGroupCommitEntry) {
            step("make-current")
            durable = entry.epochValue!!
        }

        override fun announce(entry: DeviceGroupCommitEntry) {
            log += "announce"
        }

        override suspend fun deliverWelcome(entry: DeviceGroupCommitEntry) {
            step("welcome")
        }
    }

    private fun b64(s: String) = Base64.getEncoder().encodeToString(s.toByteArray())

    private fun entry(epoch: ULong, welcome: Boolean = true) = DeviceGroupCommitEntry(
        kind = if (welcome) DeviceGroupCommitEntry.Kind.ADD else DeviceGroupCommitEntry.Kind.REMOVE,
        journalIdB64 = b64("journal"),
        epoch = epoch.toString(),
        epochKeyB64 = b64("key"),
        rowB64 = b64("row-$epoch"),
        welcomeB64 = if (welcome) b64("welcome") else null,
        welcomeRendezvousB64 = if (welcome) b64("rendezvous") else null,
        nextSnapshotB64 = b64("snap"),
        createdAtMs = 1,
    )

    @Test
    fun `entry beside N, the commit, N+1 current, the Welcome, the entry deleted`() = runTest {
        val m = Model()
        val out = runDeviceGroupCommit(entry(6uL), FakeJournalSigner(5uL), m)
        assertEquals(DeviceGroupCommitOutcome.Landed(1), out)
        assertEquals(listOf("save", "commit", "make-current", "announce", "welcome", "delete"), m.log)
        assertEquals(6uL, m.durable)
        assertNull(m.slot)
        assertEquals(listOf(6uL), m.journal.map { it.first })
    }

    @Test
    fun `a failed entry write aborts with nothing sent`() {
        val m = Model()
        m.failNext["save"] = 1
        assertThrows(StepFailed::class.java) {
            runBlocking { runDeviceGroupCommit(entry(6uL), FakeJournalSigner(5uL), m) }
        }
        assertTrue(m.journal.isEmpty())
        assertEquals(5uL, m.durable)
    }

    @Test
    fun `a proof of another epoch is refused before anything is written`() {
        val m = Model()
        assertThrows(DeviceGroupProofEpochMismatch::class.java) {
            runBlocking { runDeviceGroupCommit(entry(6uL), FakeJournalSigner(4uL), m) }
        }
        assertTrue(m.log.isEmpty())
    }

    @Test
    fun `Context path 1 - a crash after the DS took the commit is finished by the restart`() = runTest {
        val m = Model()
        m.failNext["make-current"] = 1
        try {
            runDeviceGroupCommit(entry(6uL), FakeJournalSigner(5uL), m)
        } catch (_: StepFailed) {
        }
        assertEquals(5uL, m.durable)
        assertEquals(1, m.journal.size)
        assertNotNull(m.slot)

        val out = resendOwedDeviceGroupCommit(m)
        assertEquals(DeviceGroupCommitOutcome.Landed(1), out)
        assertEquals(6uL, m.durable)
        assertEquals("nothing appended twice", 1, m.journal.size)
        assertNull(m.slot)
    }

    @Test
    fun `Context path 2 - a failed publish keeps the entry and the retry lands it`() = runTest {
        val m = Model()
        m.failNext["commit"] = 1
        try {
            runDeviceGroupCommit(entry(6uL), FakeJournalSigner(5uL), m)
        } catch (_: StepFailed) {
        }
        assertEquals(5uL, m.durable)
        assertEquals("6", m.slot?.epoch)
        assertTrue(m.journal.isEmpty())
        assertEquals(DeviceGroupCommitOutcome.Landed(1), resendOwedDeviceGroupCommit(m))
        assertEquals(6uL, m.durable)
    }

    @Test
    fun `Context path 3 - making N+1 current fails closed and the Welcome waits`() = runTest {
        val m = Model()
        m.failNext["make-current"] = 1
        try {
            runDeviceGroupCommit(entry(6uL), FakeJournalSigner(5uL), m)
        } catch (_: StepFailed) {
        }
        assertFalse(m.log.contains("welcome"))
        assertNotNull(m.slot)
        resendOwedDeviceGroupCommit(m)
        assertEquals(6uL, m.durable)
        assertEquals(1, m.log.count { it == "welcome" })
    }

    @Test
    fun `a Welcome that fails after N+1 is current is delivered without writing N+1 again`() = runTest {
        val m = Model()
        m.failNext["welcome"] = 1
        try {
            runDeviceGroupCommit(entry(6uL), FakeJournalSigner(5uL), m)
        } catch (_: StepFailed) {
        }
        assertEquals(6uL, m.durable)
        m.log.clear()
        resendOwedDeviceGroupCommit(m)
        assertEquals(listOf("commit", "announce", "welcome", "delete"), m.log)
    }

    @Test
    fun `409 deletes the entry and the device stays at N`() = runTest {
        val m = Model()
        m.takeEpochElsewhere(6uL)
        val out = runDeviceGroupCommit(entry(6uL), FakeJournalSigner(5uL), m)
        assertEquals(DeviceGroupCommitOutcome.Lost, out)
        assertNull(m.slot)
        assertEquals(5uL, m.durable)
        assertFalse(m.log.contains("make-current"))
        assertFalse(m.log.contains("welcome"))
    }

    @Test
    fun `a retry that meets 409 deletes the entry too`() = runTest {
        val m = Model()
        m.failNext["commit"] = 1
        try {
            runDeviceGroupCommit(entry(6uL), FakeJournalSigner(5uL), m)
        } catch (_: StepFailed) {
        }
        m.takeEpochElsewhere(6uL)
        assertEquals(DeviceGroupCommitOutcome.Lost, resendOwedDeviceGroupCommit(m))
        assertNull(m.slot)
        assertEquals(5uL, m.durable)
    }

    @Test
    fun `nothing owed is a no-op`() = runTest {
        val m = Model()
        assertNull(resendOwedDeviceGroupCommit(m))
        assertTrue(m.log.isEmpty())
    }

    @Test
    fun `two commits of one account never run at once`() = runTest {
        val order = mutableListOf<String>()
        val gate = CompletableDeferred<Unit>()
        val a = async {
            DeviceGroupCommitLock.withLock("acc-lock-test") {
                order += "a-start"
                gate.await()
                order += "a-end"
            }
        }
        yield()
        val b = async { DeviceGroupCommitLock.withLock("acc-lock-test") { order += "b" } }
        repeat(5) { yield() }
        assertEquals(listOf("a-start"), order)
        gate.complete(Unit)
        a.await()
        b.await()
        assertEquals(listOf("a-start", "a-end", "b"), order)
    }

    @Test
    fun `an entry round-trips and a malformed one decodes to null`() {
        val e = entry(9uL).copy(priorLeaves = listOf(0L, 2L))
        assertEquals(e, DeviceGroupCommitEntry.decode(e.encoded()))
        assertNull(DeviceGroupCommitEntry.decode("{}".toByteArray()))
        assertNull(DeviceGroupCommitEntry.decode("not json".toByteArray()))
        val half = entry(9uL, welcome = false).copy(welcomeB64 = b64("x"))
        assertNull("a Welcome without its rendezvous", DeviceGroupCommitEntry.decode(half.encoded()))
    }
}
