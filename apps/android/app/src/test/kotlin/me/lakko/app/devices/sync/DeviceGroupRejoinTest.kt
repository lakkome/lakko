// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import kotlinx.coroutines.test.runTest
import me.lakko.app.devices.DeviceGroupSnapshotStore
import me.lakko.app.devices.SnapshotGenerations
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0081 decisions 3 and 4 — a device that lost or fell behind its
 * DeviceGroup rejoins through a sibling; a stale handle's snapshot write is
 * refused. Mirrors `apps/web/src/devices/device-group-rejoin.test.ts`.
 */
class DeviceGroupRejoinTest {

    private val sibling = "aa"
    private val lost = "bb"
    private val other = "cc"

    @Test
    fun `behind is a drained read still behind the DS with no own commit in flight`() {
        assertTrue(isBehind(5uL, drained = true, latestEpoch = 7uL, commitPending = false))
        assertFalse(isBehind(5uL, drained = true, latestEpoch = 5uL, commitPending = false))
        assertFalse("rows still to read", isBehind(5uL, drained = false, latestEpoch = 7uL, commitPending = false))
        assertFalse("an unclaimed journal", isBehind(5uL, drained = true, latestEpoch = null, commitPending = false))
        assertFalse("its own commit in flight", isBehind(5uL, drained = true, latestEpoch = 6uL, commitPending = true))
    }

    private fun input(
        roster: List<RosterMember> = listOf(RosterMember(0u, sibling), RosterMember(1u, lost), RosterMember(2u, other)),
        ownKeyHex: String = sibling,
        handled: Set<Long> = emptySet(),
        watchingSinceMs: Long = 500_000,
        lastAddedAtMs: Long? = null,
        listed: Boolean = true,
        policy: Boolean = false,
        nowMs: Long = 1_000_100,
    ) = AdmissionInput(
        knockId = 10, knockAtMs = 1_000_000, keyHex = lost, ownKeyHex = ownKeyHex, roster = roster,
        handled = handled, watchingSinceMs = watchingSinceMs, lastAddedAtMs = lastAddedAtMs, listed = listed,
        policyRequiresAttestation = policy, nowMs = nowMs,
    )

    @Test
    fun `a lost device is admitted whether or not its previous leaf is in the roster`() {
        assertEquals(AdmissionVerdict.ADMIT, admissionVerdict(input()))
        assertEquals(
            AdmissionVerdict.ADMIT,
            admissionVerdict(input(roster = listOf(RosterMember(0u, sibling), RosterMember(2u, other)))),
        )
    }

    @Test
    fun `a removed device is never admitted`() {
        // Deleted from the directory and tombstoned there (ADR-0074): not listed.
        assertEquals(AdmissionVerdict.NOT_LISTED, admissionVerdict(input(listed = false)))
    }

    @Test
    fun `a knock is not answered twice`() {
        assertEquals(AdmissionVerdict.ALREADY_ADMITTED, admissionVerdict(input(lastAddedAtMs = 1_000_050)))
        assertEquals(AdmissionVerdict.HANDLED, admissionVerdict(input(handled = setOf(10L))))
        assertEquals(AdmissionVerdict.ADMIT, admissionVerdict(input(lastAddedAtMs = 900_000)))
        assertEquals(AdmissionVerdict.BEFORE_WATCHING, admissionVerdict(input(watchingSinceMs = 1_000_001)))
    }

    @Test
    fun `the lowest other leaf admits at once, any other sibling after the fallback`() {
        assertEquals(AdmissionVerdict.NOT_MINE_YET, admissionVerdict(input(ownKeyHex = other)))
        assertEquals(
            AdmissionVerdict.ADMIT,
            admissionVerdict(input(ownKeyHex = other, nowMs = 1_000_000 + FALLBACK_ADMIT_AFTER_MS)),
        )
        val roster = listOf(RosterMember(0u, lost), RosterMember(1u, sibling))
        assertEquals(AdmissionVerdict.ADMIT, admissionVerdict(input(roster = roster)))
        assertEquals(AdmissionVerdict.OWN_KEY, admissionVerdict(input(roster = roster, ownKeyHex = lost)))
    }

    @Test
    fun `under the attested-removal policy a present previous leaf waits for the person`() {
        assertEquals(AdmissionVerdict.NEEDS_ATTESTATION, admissionVerdict(input(policy = true)))
        assertEquals(
            AdmissionVerdict.ADMIT,
            admissionVerdict(input(policy = true, roster = listOf(RosterMember(0u, sibling)))),
        )
    }

    @Test
    fun `the record keeps the latest addition and a bounded handled set`() {
        var r = AdmissionRecord(watchingSinceMs = 5)
        r = r.noteAdded(lost, 100).noteAdded(lost, 50)
        assertEquals(100L, r.lastAddedAtMs[lost])
        for (i in 0 until 300) r = r.noteHandled(i.toLong())
        assertEquals(256, r.handled.size)
        assertEquals(44L, r.handled.first())
        assertEquals(r, AdmissionRecord.decode(r.encoded()))
        assertNull(AdmissionRecord.decode("""{"v":2,"watchingSinceMs":1}""".toByteArray()))
    }

    private class RunModel(private val welcomeAfter: Int?) : RejoinPorts {
        var now = 1_000L
        var attempts = 0
        val log = mutableListOf<String>()
        var state: RejoinState? = null

        override fun dropLocalState() {
            log += "drop"
        }

        override suspend fun publishDirectoryKeyPackage() {
            log += "directory-kp"
        }

        override fun loadState(): RejoinState? = state

        override fun saveState(state: RejoinState) {
            this.state = state
        }

        override suspend fun knock(nowMs: Long): RejoinState {
            log += "knock"
            return RejoinState(rendezvousB64 = "r$nowMs", joinerSnapshotB64 = "s", knockedAtMs = nowMs)
        }

        override suspend fun awaitWelcome(state: RejoinState, deadlineMs: Long): ByteArray? {
            attempts += 1
            val after = welcomeAfter
            if (after != null && attempts > after) return byteArrayOf(1)
            now = deadlineMs
            return null
        }

        override suspend fun join(state: RejoinState, welcome: ByteArray) {
            log += "join"
        }

        override fun clearState() {
            log += "clear"
            state = null
        }

        override fun now(): Long = now
    }

    @Test
    fun `the device drops its state, publishes its KeyPackage, knocks and joins`() = runTest {
        val m = RunModel(welcomeAfter = 0)
        assertEquals(RejoinPass.JOINED, rejoinOnce(m, firstPass = true))
        assertEquals(listOf("drop", "directory-kp", "knock", "join", "clear"), m.log)
    }

    @Test
    fun `it waits, then knocks again after the re-knock time`() = runTest {
        val m = RunModel(welcomeAfter = 1)
        assertEquals(RejoinPass.WAITING, rejoinOnce(m, firstPass = true))
        assertEquals(listOf("drop", "directory-kp", "knock"), m.log)
        assertEquals(RejoinPass.JOINED, rejoinOnce(m, firstPass = false))
        assertEquals(2, m.log.count { it == "knock" })
    }

    @Test
    fun `a restart resumes the knock it already made`() = runTest {
        val m = RunModel(welcomeAfter = 0)
        m.state = RejoinState(rendezvousB64 = "kept", joinerSnapshotB64 = "s", knockedAtMs = 1_000)
        assertEquals(RejoinPass.JOINED, rejoinOnce(m, firstPass = false))
        assertEquals(listOf("join", "clear"), m.log)
    }

    @Test
    fun `the re-knock rule and the state codec`() {
        val s = RejoinState(rendezvousB64 = "r", joinerSnapshotB64 = "s", knockedAtMs = 10)
        assertTrue(RejoinState.shouldKnockAgain(null, 0))
        assertFalse(RejoinState.shouldKnockAgain(s, 10 + REKNOCK_AFTER_MS - 1))
        assertTrue(RejoinState.shouldKnockAgain(s, 10 + REKNOCK_AFTER_MS))
        assertTrue("a clock that went back", RejoinState.shouldKnockAgain(s, 5))
        assertEquals(s, RejoinState.decode(s.encoded()))
        assertNull(RejoinState.decode("[]".toByteArray()))
    }

    @Test
    fun `decision 3 - a conditional write lands only on the generation it read`() {
        SnapshotGenerations.resetForTest()
        val key = "dir|acct"
        val written = mutableListOf<String>()
        val g1 = SnapshotGenerations.write(key, null) { written += "N" }
        assertEquals(g1, SnapshotGenerations.current(key))
        // Another handle made the commit's N+1 current.
        val g2 = SnapshotGenerations.write(key, null) { written += "N+1" }
        assertEquals(g1 + 1, g2)
        // The long-lived handle, still at N, tries to persist a batch.
        assertThrows(DeviceGroupSnapshotStore.SnapshotChanged::class.java) {
            SnapshotGenerations.write(key, g1) { written += "N again" }
        }
        assertEquals(listOf("N", "N+1"), written)
        // On the current generation it lands.
        SnapshotGenerations.write(key, g2) { written += "N+1 batch" }
        assertEquals(listOf("N", "N+1", "N+1 batch"), written)
        // A write that throws does not move the generation.
        val before = SnapshotGenerations.current(key)
        assertThrows(IllegalStateException::class.java) {
            SnapshotGenerations.write(key, before) { throw IllegalStateException("disk") }
        }
        assertEquals(before, SnapshotGenerations.current(key))
    }
}
