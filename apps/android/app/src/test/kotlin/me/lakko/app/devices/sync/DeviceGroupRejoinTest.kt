// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import kotlinx.coroutines.async
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
        proofCovers: Boolean = false,
        alreadyIn: Boolean = false,
    ) = AdmissionInput(
        knockId = 10, knockAtMs = 1_000_000, keyHex = lost, ownKeyHex = ownKeyHex, roster = roster,
        handled = handled, watchingSinceMs = watchingSinceMs, lastAddedAtMs = lastAddedAtMs, listed = listed,
        policyRequiresAttestation = policy, nowMs = nowMs,
        selfRemovalProofCovers = proofCovers, keyPackageAlreadyIn = alreadyIn,
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
    fun `every running sibling admits at once, with no designated admitter (third amendment)`() {
        // Not the lowest other leaf, and the knock is a moment old: admits.
        assertEquals(AdmissionVerdict.ADMIT, admissionVerdict(input(ownKeyHex = other, nowMs = 1_000_001)))
        assertEquals(AdmissionVerdict.ADMIT, admissionVerdict(input(ownKeyHex = sibling, nowMs = 1_000_001)))
        val roster = listOf(RosterMember(0u, lost), RosterMember(1u, sibling))
        assertEquals(AdmissionVerdict.ADMIT, admissionVerdict(input(roster = roster)))
        assertEquals(AdmissionVerdict.OWN_KEY, admissionVerdict(input(roster = roster, ownKeyHex = lost)))
    }

    @Test
    fun `under the policy a self-removal proof that covers the replacement admits (ADR-0081 amendment)`() {
        assertEquals(AdmissionVerdict.ADMIT, admissionVerdict(input(policy = true, proofCovers = true)))
    }

    @Test
    fun `a knock whose KeyPackage is already a leaf was answered, whatever the roster's keys say`() {
        assertEquals(AdmissionVerdict.ALREADY_ADMITTED, admissionVerdict(input(alreadyIn = true)))
        assertEquals(AdmissionVerdict.ALREADY_ADMITTED, admissionVerdict(input(alreadyIn = true, proofCovers = true)))
    }

    @Test
    fun `under the attested-removal policy a present previous leaf without a covering proof waits for the person`() {
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

    private class RunModel(
        private val welcomeAfter: Int?,
        /** What each knock's answer window brings, in order (the last one
         *  repeats); a Welcome in the window is `welcome`. */
        private val windows: List<String> = listOf("present"),
        var loginHandleKnown: Boolean = true,
    ) : RejoinPorts {
        var now = 1_000L
        var attempts = 0
        var knocks = 0
        val log = mutableListOf<String>()
        var state: RejoinState? = null
        var answeredRecorded = 0

        override fun recordAnswered() {
            answeredRecorded += 1
        }

        override fun dropLocalState() {
            log += "drop"
        }

        override suspend fun publishDirectoryKeyPackage() {
            log += "directory-kp"
        }

        override fun loadState(): RejoinState? = state

        override fun hasLoginHandle(): Boolean = loginHandleKnown

        override fun saveState(state: RejoinState) {
            this.state = state
        }

        override suspend fun knockAndListen(nowMs: Long): KnockAnswer {
            log += "knock"
            val made = RejoinState(rendezvousB64 = "r$nowMs", joinerSnapshotB64 = "s", knockedAtMs = nowMs)
            state = made
            val seen = windows[minOf(knocks, windows.size - 1)]
            knocks += 1
            val answer = when (seen) {
                "present" -> RendezvousAnswer.Present
                "welcome" -> RendezvousAnswer.Welcome(byteArrayOf(9))
                else -> null
            }
            return KnockAnswer(made, answer)
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
    fun `a restart resumes a knock a device answered`() = runTest {
        val m = RunModel(welcomeAfter = 0)
        m.state = RejoinState(rendezvousB64 = "kept", joinerSnapshotB64 = "s", knockedAtMs = 1_000, answered = true)
        assertEquals(RejoinPass.JOINED, rejoinOnce(m, firstPass = false))
        assertEquals(listOf("join", "clear"), m.log)
    }

    @Test
    fun `a restart during a knock nobody had answered yet knocks again`() = runTest {
        val m = RunModel(welcomeAfter = 0, windows = listOf("silence"))
        m.state = RejoinState(rendezvousB64 = "kept", joinerSnapshotB64 = "s", knockedAtMs = 1_000)
        assertEquals(RejoinPass.FOUND, rejoinOnce(m, firstPass = false))
        assertEquals(listOf("knock", "clear"), m.log)
    }

    @Test
    fun `a presence note means wait for the Welcome with no answer deadline`() = runTest {
        val m = RunModel(welcomeAfter = 0, windows = listOf("present"))
        assertEquals(RejoinPass.JOINED, rejoinOnce(m, firstPass = true))
        assertEquals(listOf("drop", "directory-kp", "knock", "join", "clear"), m.log)
        assertEquals(1, m.attempts)
    }

    @Test
    fun `silence in the answer window founds, without waiting for a Welcome`() = runTest {
        val m = RunModel(welcomeAfter = null, windows = listOf("silence"))
        assertEquals(RejoinPass.FOUND, rejoinOnce(m, firstPass = true))
        assertEquals(listOf("drop", "directory-kp", "knock", "clear"), m.log)
        assertEquals("no Welcome wait after silence", 0, m.attempts)
        assertNull(m.state)
    }

    @Test
    fun `a device that knows no username neither knocks nor founds, and knocks once a name is known`() = runTest {
        // 2026-11-15, seen on an iPhone: a device with no login handle while
        // another device of the account was listed founded a second
        // DeviceGroup, splitting the account.
        val m = RunModel(welcomeAfter = null, windows = listOf("welcome"), loginHandleKnown = false)
        assertEquals(RejoinPass.NO_LOGIN_HANDLE, rejoinOnce(m, firstPass = true))
        assertEquals(RejoinPass.NO_LOGIN_HANDLE, rejoinOnce(m, firstPass = false))
        assertEquals(listOf("drop", "directory-kp"), m.log)
        assertEquals("no knock without a handle", 0, m.knocks)
        assertEquals(0, m.attempts)
        m.loginHandleKnown = true
        assertEquals(RejoinPass.JOINED, rejoinOnce(m, firstPass = false))
        assertEquals(listOf("drop", "directory-kp", "knock", "join", "clear"), m.log)
    }

    @Test
    fun `a Welcome inside the answer window joins at once`() = runTest {
        val m = RunModel(welcomeAfter = null, windows = listOf("welcome"))
        assertEquals(RejoinPass.JOINED, rejoinOnce(m, firstPass = true))
        assertEquals(listOf("drop", "directory-kp", "knock", "join", "clear"), m.log)
        assertEquals(0, m.attempts)
    }

    @Test
    fun `a device that answered and then went quiet is asked again at the re-knock, and silence then founds`() = runTest {
        val m = RunModel(welcomeAfter = null, windows = listOf("present", "silence"))
        assertEquals(RejoinPass.WAITING, rejoinOnce(m, firstPass = true))
        assertEquals(RejoinPass.FOUND, rejoinOnce(m, firstPass = false))
        assertEquals(2, m.log.count { it == "knock" })
    }

    @Test
    fun `a presence note or a Welcome in the window is recorded as answered`() = runTest {
        val present = RunModel(welcomeAfter = 0, windows = listOf("present"))
        assertEquals(RejoinPass.JOINED, rejoinOnce(present, firstPass = true))
        assertEquals(1, present.answeredRecorded)
        val welcome = RunModel(welcomeAfter = null, windows = listOf("welcome"))
        assertEquals(RejoinPass.JOINED, rejoinOnce(welcome, firstPass = true))
        assertEquals(1, welcome.answeredRecorded)
    }

    @Test
    fun `silence or no username is never recorded as answered`() = runTest {
        val silence = RunModel(welcomeAfter = null, windows = listOf("silence"))
        assertEquals(RejoinPass.FOUND, rejoinOnce(silence, firstPass = true))
        assertEquals(0, silence.answeredRecorded)
        val noHandle = RunModel(welcomeAfter = null, loginHandleKnown = false)
        assertEquals(RejoinPass.NO_LOGIN_HANDLE, rejoinOnce(noHandle, firstPass = true))
        assertEquals(0, noHandle.answeredRecorded)
    }

    @Test
    fun `the answer window is four knock round trips, at least one second`() {
        assertEquals(1_000L, rejoinAnswerWindowMs(0))
        assertEquals(1_000L, rejoinAnswerWindowMs(250))
        assertEquals(1_200L, rejoinAnswerWindowMs(300))
        assertEquals(8_000L, rejoinAnswerWindowMs(2_000))
        assertEquals("a clock that went back", 1_000L, rejoinAnswerWindowMs(-50))
    }

    @Test
    fun `a Welcome wins over a presence note, and nothing is no answer`() {
        val present = byteArrayOf(1)
        val isPresent: (ByteArray) -> Boolean = { it.contentEquals(present) }
        assertNull(rendezvousAnswer(emptyList(), isPresent))
        assertEquals(RendezvousAnswer.Present, rendezvousAnswer(listOf(present, present), isPresent))
        val got = rendezvousAnswer(listOf(present, byteArrayOf(7, 7)), isPresent)
        assertTrue(got is RendezvousAnswer.Welcome)
        assertTrue((got as RendezvousAnswer.Welcome).bytes.contentEquals(byteArrayOf(7, 7)))
    }

    @Test
    fun `a backlog knock is answered only while its device can still be waiting`() {
        assertTrue(shouldAnswerBacklogKnock(1_000_000, 1_000_000 + KNOCK_BACKLOG_ANSWER_MS))
        assertFalse(shouldAnswerBacklogKnock(1_000_000, 1_000_000 + KNOCK_BACKLOG_ANSWER_MS + 1))
        assertFalse("unknown time", shouldAnswerBacklogKnock(null, 1_000_000))
    }

    @Test
    fun `a journal below this device's epoch was reset under it`() {
        assertTrue(isJournalReset(5uL, drained = true, latestEpoch = 1uL))
        assertFalse(isJournalReset(5uL, drained = true, latestEpoch = 5uL))
        assertFalse(isJournalReset(5uL, drained = false, latestEpoch = 1uL))
        assertFalse(isJournalReset(5uL, drained = true, latestEpoch = null))
    }

    @Test
    fun `a DS error body yields its code and never anything else`() {
        assertEquals("anon-token-required", dsErrorCode("""{"error":"anon-token-required"}"""))
        assertNull(dsErrorCode("""{"error":"Group 0a1b not found here"}"""))
        assertNull(dsErrorCode("plain text"))
    }

    @Test
    fun `the re-knock rule and the state codec`() {
        val s = RejoinState(rendezvousB64 = "r", joinerSnapshotB64 = "s", knockedAtMs = 10)
        assertTrue(RejoinState.shouldKnockAgain(null, 0))
        assertTrue("never answered", RejoinState.shouldKnockAgain(s, 11))
        val answered = s.copy(answered = true)
        assertFalse(RejoinState.shouldKnockAgain(answered, 10 + REKNOCK_AFTER_MS - 1))
        assertTrue(RejoinState.shouldKnockAgain(answered, 10 + REKNOCK_AFTER_MS))
        assertTrue("a clock that went back", RejoinState.shouldKnockAgain(answered, 5))
        assertEquals(answered, RejoinState.decode(answered.encoded()))
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

    @Test
    fun `a rejoin waiting for a login handle wakes when the username is learned`() = runTest {
        // 2026-11-15 - a passkey sign-in learns the name from its prompt: the
        // waiting rejoin knocks at once instead of at its next pass.
        val revision = kotlinx.coroutines.flow.MutableStateFlow(4L)
        val seen = revision.value
        val waiter = async { awaitLoginHandleOrRetry(revision, seen) }
        testScheduler.advanceTimeBy(1_000L)
        assertFalse(waiter.isCompleted)
        revision.value = seen + 1
        assertTrue("woken by the learned name", waiter.await())
        assertTrue("well before the retry interval", testScheduler.currentTime < NO_LOGIN_HANDLE_RETRY_MS)
    }

    @Test
    fun `a name learned during the pass that found none is not missed`() = runTest {
        val revision = kotlinx.coroutines.flow.MutableStateFlow(0L)
        val seen = revision.value
        // Learned while the pass was running, before the wait started.
        revision.value = 1L
        assertTrue(awaitLoginHandleOrRetry(revision, seen))
        assertEquals(0L, testScheduler.currentTime)
    }

    @Test
    fun `with no name learned the wait ends at the retry interval`() = runTest {
        val revision = kotlinx.coroutines.flow.MutableStateFlow(0L)
        assertFalse(awaitLoginHandleOrRetry(revision, revision.value))
        assertEquals(NO_LOGIN_HANDLE_RETRY_MS, testScheduler.currentTime)
    }

    @Test
    fun `a rejoin with no handle knocks on the pass after the name was adopted`() = runTest {
        val m = RunModel(welcomeAfter = null, windows = listOf("welcome"), loginHandleKnown = false)
        val revision = kotlinx.coroutines.flow.MutableStateFlow(0L)
        val seen = revision.value
        assertEquals(RejoinPass.NO_LOGIN_HANDLE, rejoinOnce(m, firstPass = true))
        // The prompt's name is adopted: the provider now answers, the wait wakes.
        m.loginHandleKnown = true
        revision.value = 1L
        assertTrue(awaitLoginHandleOrRetry(revision, seen))
        assertEquals(RejoinPass.JOINED, rejoinOnce(m, firstPass = false))
        assertEquals(1, m.knocks)
    }
}
