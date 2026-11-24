// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.ds.MailboxMessage
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0091 phase 2 — the client half of a device that fell behind coming back.
 *
 * Everything here is pure: the core's fold and admission rule are injected
 * stand-ins that implement section 1's and section 4's tables, so what is
 * tested is what THIS client feeds them and does with their answers. The
 * native rules themselves are tested in `lakko-mls`; the FFI calls in
 * `ChatRepository` (minting K, `joinReentry`, `proposeReentryReplacement`,
 * the sends) need the native core and a network and are not run here.
 */
class ConversationReentryTest {

    private class Clock(var now: Long = 1_000_000L) {
        fun read(): Long = now
    }

    private class MemoryBehindStore : ConversationBehindStore {
        val saved = HashMap<String, Map<String, ConversationBehindEntry>>()
        var refuse = false

        override fun load(accountIdB64: String) = saved[accountIdB64].orEmpty()

        override fun save(accountIdB64: String, entries: Map<String, ConversationBehindEntry>): Boolean {
            if (refuse) return false
            saved[accountIdB64] = entries
            return true
        }

        override fun clearForAccount(accountIdB64: String) {
            saved.remove(accountIdB64)
        }
    }

    /** Section 1's table, as the core implements it (stand-in). */
    private val sectionOneVerdict = BehindVerdict { previous, o, now ->
        val clears = o.drained && o.latestEpoch != null && o.latestEpoch <= o.localEpoch
        if (clears) return@BehindVerdict BehindState.IN_STEP
        val suspects = !o.ownCommitOwed &&
            ((o.drained && o.latestEpoch != null && o.latestEpoch > o.localEpoch) || o.refusedReads >= 3)
        when (previous.level) {
            BehindLevel.CONFIRMED -> previous
            BehindLevel.SUSPECTED -> {
                val evidence = o.failedRowAboveLocal || o.publishRefused || o.refusedReads >= 3 ||
                    now - previous.sinceMs >= 600_000L
                if (evidence) BehindState(BehindLevel.CONFIRMED, previous.sinceMs) else previous
            }
            BehindLevel.IN_STEP -> if (!suspects) {
                previous
            } else if (o.failedRowAboveLocal || o.publishRefused || o.refusedReads >= 3) {
                BehindState(BehindLevel.CONFIRMED, now)
            } else {
                BehindState(BehindLevel.SUSPECTED, now)
            }
        }
    }

    private fun tracker(store: MemoryBehindStore = MemoryBehindStore(), clock: Clock = Clock()) =
        ConversationBehindTracker(store, sectionOneVerdict, clock::read).also { it.bind("acct") }

    // -----------------------------------------------------------------------
    // Detection
    // -----------------------------------------------------------------------

    @Test
    fun `a drained read naming a later epoch suspects the device and stops it acting`() {
        val t = tracker()
        val s = t.observe("AB", ownCommitOwed = false, drained = true, latestEpoch = 7uL, localEpoch = 6uL)
        assertEquals(BehindLevel.SUSPECTED, s.level)
        assertTrue(t.stopsActing("ab"))
        assertFalse("suspected is not shown", t.isConfirmed("ab"))
        assertEquals("the read's epoch becomes the request's epoch_left", 7L, t.entry("ab").latestEpoch)
    }

    @Test
    fun `an own commit owed or an undrained read never suspects`() {
        val t = tracker()
        assertEquals(BehindLevel.IN_STEP, t.observe("ab", true, true, 7uL, 6uL).level)
        assertEquals(BehindLevel.IN_STEP, t.observe("ab", false, false, 7uL, 6uL).level)
        assertFalse(t.stopsActing("ab"))
    }

    @Test
    fun `a skipped row framed above the local epoch is evidence, and an applied row resets it`() {
        val t = tracker()
        t.noteSkipped("ab", sealedEpoch = 7uL, localEpoch = 6uL, rowAtMs = 500L)
        assertTrue(t.realProgressEvidence("ab"))
        assertEquals("the first unreadable row's time starts the gap", 500L, t.entry("ab").gapFromMs)
        t.noteSkipped("ab", sealedEpoch = 8uL, localEpoch = 6uL, rowAtMs = 900L)
        assertEquals("the FIRST time is kept", 500L, t.entry("ab").gapFromMs)
        t.noteApplied("ab")
        assertFalse(t.entry("ab").failedRowAboveLocal)
        assertEquals("the gap start is not reset by an applied row", 500L, t.entry("ab").gapFromMs)
    }

    @Test
    fun `a skipped row at or below the local epoch is not evidence`() {
        val t = tracker()
        t.noteSkipped("ab", sealedEpoch = 6uL, localEpoch = 6uL, rowAtMs = 10L)
        t.noteSkipped("ab", sealedEpoch = null, localEpoch = 6uL, rowAtMs = 10L)
        assertFalse(t.realProgressEvidence("ab"))
    }

    @Test
    fun `with evidence the suspicion is confirmed at once, and survives a restart`() {
        val store = MemoryBehindStore()
        val t = tracker(store)
        t.noteSkipped("ab", 7uL, 6uL, 10L)
        assertEquals(BehindLevel.CONFIRMED, t.observe("ab", false, true, 7uL, 6uL).level)
        val again = ConversationBehindTracker(store, sectionOneVerdict).also { it.bind("acct") }
        assertTrue("confirmed is a durable flag", again.isConfirmed("ab"))
    }

    @Test
    fun `three refused reads suspect the device without a drained read`() {
        val t = tracker()
        t.noteReadRefused("ab", ownCommitOwed = false, localEpoch = 6uL)
        t.noteReadRefused("ab", ownCommitOwed = false, localEpoch = 6uL)
        assertFalse(t.stopsActing("ab"))
        t.noteReadRefused("ab", ownCommitOwed = false, localEpoch = 6uL)
        assertTrue(t.stopsActing("ab"))
        assertTrue(t.realProgressEvidence("ab"))
    }

    @Test
    fun `a read that answers resets the refused reads`() {
        val t = tracker()
        t.noteReadRefused("ab", false, 6uL)
        t.noteReadRefused("ab", false, 6uL)
        t.observe("ab", false, false, null, 6uL)
        t.noteReadRefused("ab", false, 6uL)
        assertFalse(t.stopsActing("ab"))
    }

    @Test
    fun `a drained read at the local epoch clears the suspicion and everything it rested on`() {
        val t = tracker()
        t.noteSkipped("ab", 7uL, 6uL, 10L)
        t.observe("ab", false, true, 7uL, 6uL)
        assertTrue(t.stopsActing("ab"))
        t.observe("ab", false, true, 7uL, 7uL)
        assertFalse(t.stopsActing("ab"))
        assertNull(t.entry("ab").gapFromMs)
        assertFalse(t.realProgressEvidence("ab"))
    }

    @Test
    fun `the ten minute rule confirms a suspicion with no other evidence`() {
        val clock = Clock()
        val t = tracker(clock = clock)
        t.observe("ab", false, true, 7uL, 6uL)
        clock.now += 599_000L
        assertFalse(t.observe("ab", false, true, 7uL, 6uL).level == BehindLevel.CONFIRMED)
        clock.now += 2_000L
        assertEquals(BehindLevel.CONFIRMED, t.observe("ab", false, true, 7uL, 6uL).level)
    }

    @Test
    fun `coming back forgets the suspicion and the newest row, and the join is the last good read`() {
        val store = MemoryBehindStore()
        val t = tracker(store)
        t.noteRowKnown("ab", 400L)
        t.noteSkipped("ab", 7uL, 6uL, 10L)
        t.observe("ab", false, true, 7uL, 6uL)
        t.cameBack("AB", 2_000_000L)
        assertFalse(t.stopsActing("ab"))
        val kept = store.saved["acct"].orEmpty()["ab"]
        assertEquals(ConversationBehindEntry(lastReadOkMs = 2_000_000L), kept)
        assertEquals("applied none since joining: the join starts any later gap", 2_000_000L, t.gapStartMs("ab"))
    }

    @Test
    fun `forgetting the conversation drops the read progress too`() {
        val store = MemoryBehindStore()
        val t = tracker(store)
        t.noteRowKnown("ab", 400L)
        t.forget("AB")
        assertTrue(store.saved["acct"].orEmpty().isEmpty())
    }

    // -----------------------------------------------------------------------
    // Walk 2, R1 — where the gap starts
    // -----------------------------------------------------------------------

    @Test
    fun `with every read refused the gap starts at the newest row applied, not when it was suspected`() {
        val clock = Clock(now = 1_000_000L)
        val t = tracker(clock = clock)
        t.noteRowKnown("ab", 900_000L)
        t.observe("ab", false, false, null, 6uL)
        clock.now = 5_000_000L
        repeat(3) { t.noteReadRefused("ab", ownCommitOwed = false, localEpoch = 6uL) }
        assertTrue(t.stopsActing("ab"))
        assertNull("no row was skipped: every read was refused", t.entry("ab").gapFromMs)
        assertEquals(900_000L, t.gapStartMs("ab"))
    }

    @Test
    fun `the gap start falls back to the last good read, then the suspicion, then now`() {
        assertEquals(42L, reentryGapStart(ConversationBehindEntry(lastReadOkMs = 42L, sinceMs = 50L), 99L))
        assertEquals(50L, reentryGapStart(ConversationBehindEntry(sinceMs = 50L), 99L))
        assertEquals(99L, reentryGapStart(ConversationBehindEntry(), 99L))
        assertEquals(
            "a row known applied is preferred to the last good read",
            30L,
            reentryGapStart(ConversationBehindEntry(lastKnownRowMs = 30L, lastReadOkMs = 42L), 99L),
        )
    }

    @Test
    fun `a skipped row starts the gap only when it is the earlier`() {
        assertEquals(10L, reentryGapStart(ConversationBehindEntry(gapFromMs = 10L, lastKnownRowMs = 30L), 99L))
        assertEquals(
            "a row applied after the first skipped one never moves the start later",
            30L,
            reentryGapStart(ConversationBehindEntry(gapFromMs = 60L, lastKnownRowMs = 30L), 99L),
        )
    }

    @Test
    fun `a skipped row with no time does not start the gap at the moment of the skip`() {
        val clock = Clock(now = 7_000_000L)
        val t = tracker(clock = clock)
        t.noteRowKnown("ab", 500L)
        t.noteSkipped("ab", 7uL, 6uL, rowAtMs = null)
        assertNull(t.entry("ab").gapFromMs)
        assertEquals(500L, t.gapStartMs("ab"))
    }

    @Test
    fun `the newest known row only moves forward`() {
        val t = tracker()
        t.noteRowKnown("ab", 500L)
        t.noteRowKnown("ab", 300L)
        t.noteRowKnown("ab", null)
        assertEquals(500L, t.entry("ab").lastKnownRowMs)
        t.noteRowKnown("ab", 800L)
        assertEquals(800L, t.entry("ab").lastKnownRowMs)
    }

    @Test
    fun `the last good read is written at most once a minute`() {
        val store = MemoryBehindStore()
        val clock = Clock(now = 1_000_000L)
        val t = tracker(store, clock)
        t.observe("ab", false, false, null, 6uL)
        assertEquals(1_000_000L, t.entry("ab").lastReadOkMs)
        clock.now += 30_000L
        t.observe("ab", false, false, null, 6uL)
        assertEquals("within the minute nothing is written", 1_000_000L, t.entry("ab").lastReadOkMs)
        clock.now += 31_000L
        t.observe("ab", false, false, null, 6uL)
        assertEquals(1_061_000L, t.entry("ab").lastReadOkMs)
        assertEquals(1_061_000L, store.saved["acct"].orEmpty()["ab"]?.lastReadOkMs)
    }

    @Test
    fun `the read progress survives coming back in step`() {
        val t = tracker()
        t.noteRowKnown("ab", 500L)
        t.noteSkipped("ab", 7uL, 6uL, 10L)
        t.observe("ab", false, true, 7uL, 6uL)
        assertTrue(t.stopsActing("ab"))
        t.observe("ab", false, true, 7uL, 7uL)
        assertFalse(t.stopsActing("ab"))
        assertEquals(500L, t.entry("ab").lastKnownRowMs)
        assertNull(t.entry("ab").gapFromMs)
        assertEquals(BehindLevel.IN_STEP, t.entry("ab").level)
    }

    // -----------------------------------------------------------------------
    // Walk 2, defects 2 and 3 — a device that once swept is still suspected
    // -----------------------------------------------------------------------

    private fun outboxEntry(acked: Boolean, isSweep: Boolean) = PendingCommitEntry(
        id = "e",
        accountId = "acct",
        groupId = "g",
        targetEpoch = "2",
        commitB64 = "AA==",
        acked = acked,
        isSweep = isSweep,
    )

    @Test
    fun `refused reads suspect a device whose only outbox entry is an acked sweep, and a request is due`() {
        val t = tracker()
        // The walk: the member had once swept as admin; its entry was acked
        // and never collected, and it kept `own_commit_owed` true forever.
        val owed = ownCommitOwedBy(stagedSweep = false, entries = listOf(outboxEntry(acked = true, isSweep = true)))
        assertFalse(owed)
        var state = BehindState.IN_STEP
        repeat(3) { state = t.noteReadRefused("ab", ownCommitOwed = owed, localEpoch = 6uL) }
        assertTrue(t.stopsActing("ab"))
        assertTrue("the refused-read path asks to come back", reentryRequestAfterRefusedRead(state))
        assertTrue(
            "a confirmed device's first request is due at once",
            ReentryRequestDue { s, last, _, _ -> s.level != BehindLevel.IN_STEP && last == null }
                .due(t.entry("ab").state, null, false, 0L),
        )
    }

    // -----------------------------------------------------------------------
    // Live walk 5 (2026-11-20) - an own device that does not answer
    // -----------------------------------------------------------------------

    @Test
    fun `a request sent to the own device marks the episode, a failed one does not`() {
        val t = tracker()
        t.observe("ab", false, true, 7uL, 6uL)
        // Not sent: back-dated retry, no route.
        t.noteRequestSent("ab", 5L)
        assertFalse(t.entry("ab").ownDeviceAsked)
        assertFalse(t.entry("ab").lastRequestOwnDevice)
        t.noteRequestSent("ab", 10L, ReentryRouteKind.OWN_DEVICE)
        assertTrue(t.entry("ab").ownDeviceAsked)
        assertTrue(t.entry("ab").lastRequestOwnDevice)
        // The next request went by knock: the episode stays marked.
        t.noteRequestSent("ab", 20L, ReentryRouteKind.KNOCK)
        assertTrue(t.entry("ab").ownDeviceAsked)
        assertFalse(t.entry("ab").lastRequestOwnDevice)
    }

    @Test
    fun `an own device that cannot be reached is not waited on`() {
        // No DeviceGroup sync on this device: route 1 counts as absent.
        assertFalse(reentryOwnDeviceReachable(inConversation = true, deviceGroupSyncActive = false))
        assertTrue(reentryOwnDeviceReachable(inConversation = true, deviceGroupSyncActive = true))
        assertFalse(reentryOwnDeviceReachable(inConversation = false, deviceGroupSyncActive = true))
        // A send to it that did not go marks it unavailable for the episode,
        // so the retry takes the chat's own route; back in step clears it.
        val t = tracker()
        t.observe("ab", false, true, 7uL, 6uL)
        t.noteOwnDeviceUnavailable("ab")
        t.noteRequestSent("ab", 5L)
        assertTrue(t.entry("ab").ownDeviceAsked)
        assertFalse(t.entry("ab").lastRequestOwnDevice)
        t.observe("ab", false, true, 7uL, 7uL)
        assertFalse(t.entry("ab").ownDeviceAsked)
        val source = java.io.File("src/main/kotlin/me/lakko/app/chat/ChatRepository.kt").readText()
        val body = source.substringAfter("    private suspend fun requestReentryIfDue(groupIdHex: String) {")
            .substringBefore("\n    }\n")
        assertFalse("no silent return on a missing DeviceGroup sync", "active == null" in body)
        assertTrue("noteOwnDeviceUnavailable(groupIdHex)" in body)
    }

    @Test
    fun `back in step ends the episode and the own device is asked first again`() {
        val t = tracker()
        t.observe("ab", false, true, 7uL, 6uL)
        t.noteRequestSent("ab", 10L, ReentryRouteKind.OWN_DEVICE)
        assertEquals(BehindLevel.IN_STEP, t.observe("ab", false, true, 7uL, 7uL).level)
        assertFalse(t.entry("ab").ownDeviceAsked)
        assertFalse(t.entry("ab").lastRequestOwnDevice)
        assertEquals(null, t.entry("ab").lastRequestSentMs)
    }

    @Test
    fun `the episode marks feed the core's route and cadence`() {
        // A stand-in for the core's two rules (the host JVM has no native
        // library): route = own device unless asked; due after 2 minutes for
        // an own-device request, 30 minutes otherwise.
        val due = ReentryRequestDue { s, last, lastOwn, now ->
            s.level != BehindLevel.IN_STEP &&
                (last == null || now - last >= if (lastOwn) 120_000L else 1_800_000L)
        }
        fun route(asked: Boolean, group: Boolean) =
            if (!asked) ReentryRouteKind.OWN_DEVICE else if (group) ReentryRouteKind.KNOCK else ReentryRouteKind.MAILBOX
        for (group in listOf(true, false)) {
            val t = tracker()
            t.observe("ab", false, true, 7uL, 6uL)
            val sent = mutableListOf<ReentryRouteKind>()
            for (now in listOf(0L, 119_999L, 120_000L, 180_000L, 1_920_000L)) {
                val e = t.entry("ab")
                if (!due.due(e.state, e.lastRequestSentMs, e.lastRequestOwnDevice, now)) continue
                val r = route(e.ownDeviceAsked, group)
                sent += r
                t.noteRequestSent("ab", now, r)
            }
            val other = if (group) ReentryRouteKind.KNOCK else ReentryRouteKind.MAILBOX
            assertEquals(listOf(ReentryRouteKind.OWN_DEVICE, other, other), sent)
        }
        // The production request passes the episode marks to the core.
        val source = java.io.File("src/main/kotlin/me/lakko/app/chat/ChatRepository.kt").readText()
        val body = source.substringAfter("    private suspend fun requestReentryIfDue(groupIdHex: String) {")
            .substringBefore("\n    }\n")
        assertTrue("entry.lastRequestOwnDevice, now)" in body)
        assertTrue(
            "reentryRouteLocked(record, identity, entry.ownDeviceAsked, ownDeviceReachable = deviceGroupSync != null)" in body,
        )
        assertTrue("noteRequestSent(groupIdHex, now, route)" in body)
    }

    @Test
    fun `no request after a refused read while still in step`() {
        assertFalse(reentryRequestAfterRefusedRead(BehindState.IN_STEP))
        assertTrue(reentryRequestAfterRefusedRead(BehindState(BehindLevel.SUSPECTED, 1L)))
    }

    // -----------------------------------------------------------------------
    // Maintainer 2026-11-20 — a behind chat looks as always
    // -----------------------------------------------------------------------

    @Test
    fun `a message sent while confirmed behind is kept as not sent with the ordinary failure`() {
        // The refusal carries no text of its own: the composer shows the
        // ordinary send failure.
        assertEquals(
            "chat.send.failed",
            chatErrorText(personActionHeldError(), "chat.send.failed", { false }).key,
        )
        val source = java.io.File("src/main/kotlin/me/lakko/app/chat/ChatRepository.kt").readText()
        // The first send: the row is in the conversation first, then its
        // outbox entry, then the attempt (connection-state section 7.3).
        val send = source.substringAfter("    private suspend fun sendMessageOnMlsThread(")
            .substringBefore("\n    }\n")
        val pending = send.indexOf("record.appendPending(")
        val entry = send.indexOf("outbox.add(")
        val attempt = send.indexOf("outbox.sendNow(groupIdHex, localId)")
        assertTrue(pending in 0 until entry)
        assertTrue(entry in 0 until attempt)
        assertFalse("no waiting-note text", "chat.reentry" in send)
        // Every publish of an entry (the first send, an automatic send, a
        // Retry) checks before it publishes; a refusal there is "Not sent"
        // (the outbox engine files any non-network failure as FAILED, see
        // OutboxTest.a_refused_entry_stays_not_sent_and_survives_a_restart).
        val deliver = source.substringAfter("    private suspend fun deliverOnMlsThread(")
            .substringBefore("\n    }\n")
        val check = deliver.indexOf("requireMessageSendAllowed(groupIdHex)")
        assertTrue(check >= 0)
        assertTrue(check < deliver.indexOf("sendLocked("))
        // Retry goes through the outbox, so it fails again the same way.
        val retry = source.substringAfter("    private suspend fun retrySendOnMlsThread(")
            .substringBefore("\n    }\n")
        assertTrue("outbox.retry(groupIdHex, localId)" in retry)
    }

    @Test
    fun `nothing is kept across an unbind`() {
        val t = tracker()
        t.observe("ab", false, true, 7uL, 6uL)
        t.unbind()
        assertFalse(t.stopsActing("ab"))
    }

    // -----------------------------------------------------------------------
    // The durable record
    // -----------------------------------------------------------------------

    private fun row(n: Int) = ReentryRequestRow("kp$n", 6L, 1L, n.toLong(), ReentryRouteKind.KNOCK)

    @Test
    fun `the record keeps the newest requests and names those that left`() {
        val (first, d1) = withReentryRequest(null, "AB", row(1), kept = 2)
        assertTrue(d1.isEmpty())
        assertEquals("ab", first.groupIdHex)
        val (second, d2) = withReentryRequest(first, "ab", row(2), kept = 2)
        assertTrue(d2.isEmpty())
        val (third, d3) = withReentryRequest(second, "ab", row(3), kept = 2)
        assertEquals(listOf("kp2", "kp3"), third.requests.map { it.keyPackageB64 })
        assertEquals("the dropped one's keys are forgotten by the caller", listOf("kp1"), d3.map { it.keyPackageB64 })
    }

    @Test
    fun `a Welcome refused as still active is tried as a re-entry only while a record exists`() {
        assertTrue(tryAsReentry("AB", setOf("ab")))
        assertFalse(tryAsReentry("ab", emptySet()))
        assertFalse(tryAsReentry(null, setOf("ab")))
        assertFalse(tryAsReentry("cd", setOf("ab")))
    }

    @Test
    fun `a held Welcome is never dropped while a re-entry record exists`() {
        assertEquals(HeldWelcomeVerdict.RETRY, heldWelcomeVerdict(true, true, reentryRecorded = true))
        assertEquals(HeldWelcomeVerdict.RETRY, heldWelcomeVerdict(false, true, reentryRecorded = true))
        assertEquals(HeldWelcomeVerdict.DROP, heldWelcomeVerdict(true, true, reentryRecorded = false))
        assertEquals(HeldWelcomeVerdict.RETRY, heldWelcomeVerdict(true, false, reentryRecorded = true))
    }

    // -----------------------------------------------------------------------
    // The re-admitter
    // -----------------------------------------------------------------------

    @Test
    fun `the pre-checks - held, not behind itself, at the request epoch`() {
        assertEquals(ReentryRequestSkip.NOT_HELD, ReentryRequestCheck.check(false, false, 6uL, 6uL))
        assertEquals(ReentryRequestSkip.NOT_HELD, ReentryRequestCheck.check(true, false, null, 6uL))
        assertEquals(ReentryRequestSkip.SELF_BEHIND, ReentryRequestCheck.check(true, true, 6uL, 6uL))
        assertEquals(ReentryRequestSkip.OTHER_EPOCH, ReentryRequestCheck.check(true, false, 7uL, 6uL))
        assertNull(ReentryRequestCheck.check(true, false, 6uL, 6uL))
    }

    @Test
    fun `the ledger hands the core the facts it cannot see`() {
        val clock = Clock(10_000L)
        val ledger = ReentryAdmissionLedger(clock::read)
        val fresh = ledger.facts("ab", "LEAF", 6uL, deviceStillTheAccounts = true)
        assertEquals(ReentryAdmissionFacts(true, null, false, null), fresh)

        ledger.noteMessage("ab", "leaf", 6uL, atMs = 9_000L)
        assertEquals("a message at the current epoch counts", 9_000L, ledger.facts("AB", "leaf", 6uL, true).lastCurrentEpochMessageFromLeafMs)
        assertNull("one at another epoch does not", ledger.facts("ab", "leaf", 7uL, true).lastCurrentEpochMessageFromLeafMs)

        ledger.noteHandled("ab", "leaf", 6uL)
        assertTrue(ledger.facts("ab", "leaf", 6uL, true).handledForLeafAtThisEpoch)
        assertFalse(ledger.facts("ab", "leaf", 7uL, true).handledForLeafAtThisEpoch)

        ledger.noteAccepted("ab", "leaf")
        assertEquals(10_000L, ledger.facts("ab", "leaf", 7uL, true).lastAcceptedForLeafMs)
        ledger.clear()
        assertEquals(ReentryAdmissionFacts(false, null, false, null), ledger.facts("ab", "leaf", 6uL, false))
    }

    @Test
    fun `the directory check fails closed`() {
        assertTrue(directoryListsDevice(listOf("aa", "BB"), "bb"))
        assertFalse(directoryListsDevice(listOf("aa"), "bb"))
        assertFalse("an unreadable directory never admits", directoryListsDevice(null, "bb"))
    }

    // -----------------------------------------------------------------------
    // The knock key
    // -----------------------------------------------------------------------

    private val strictlyNewer: (Int, Int?) -> Boolean = { incoming, held -> held == null || incoming > held }

    @Test
    fun `a knock key is kept only from the owner or an admin and only when newer`() {
        assertFalse(keepKnockKey(false, 3, null, strictlyNewer))
        assertTrue(keepKnockKey(true, 0, null, strictlyNewer))
        assertTrue(keepKnockKey(true, 3, HeldKnockKey(2, "x"), strictlyNewer))
        assertFalse(keepKnockKey(true, 2, HeldKnockKey(2, "x"), strictlyNewer))
        assertFalse(keepKnockKey(true, 1, HeldKnockKey(2, "x"), strictlyNewer))
    }

    @Test
    fun `the owner re-sends the knock key on a new version and to a member it was not sent to`() {
        val two = setOf("a", "b")
        assertTrue("never sent", knockKeyResendDue(null, 0, two))
        assertFalse(knockKeyResendDue(KnockKeySentMark(0, two), 0, two))
        assertTrue("a new version", knockKeyResendDue(KnockKeySentMark(0, two), 1, two))
        assertTrue("a newcomer", knockKeyResendDue(KnockKeySentMark(1, two), 1, two + "c"))
        assertFalse("a member leaving owes nothing", knockKeyResendDue(KnockKeySentMark(1, two), 1, setOf("a")))
    }

    @Test
    fun `a growth seen while the send was skipped stays due until a send goes out`() {
        // Walk 2: the owner sent the key into the group alone; the member its
        // own sweep added was never seen as growth by a poll, so it got the
        // key only after a restart. The mark is the members at the last
        // SUCCESSFUL send: a skipped send writes no mark.
        val alone = KnockKeySentMark(0, setOf("owner"))
        val withMember = setOf("owner", "member")
        assertTrue(knockKeyResendDue(alone, 0, withMember))
        // A later poll with no growth of its own: still due.
        assertTrue(knockKeyResendDue(alone, 0, withMember))
        // Sent: the mark now holds the member.
        assertFalse(knockKeyResendDue(KnockKeySentMark(0, withMember), 0, withMember))
    }

    @Test
    fun `knock seeds go to a recipient only for the versions it was not sent`() {
        val devices = setOf("d1")
        assertEquals("never sent: every version", 0..3, knockSeedVersionsToSend(null, 3, devices))
        assertTrue("current: nothing", knockSeedVersionsToSend(KnockSeedSentMark(3, devices), 3, devices).isEmpty())
        assertEquals("a bump: only the new version", 4..4, knockSeedVersionsToSend(KnockSeedSentMark(3, devices), 4, devices))
        assertEquals(
            "a device that was not there gets every version",
            0..4,
            knockSeedVersionsToSend(KnockSeedSentMark(3, devices), 4, devices + "d2"),
        )
        assertEquals(
            "a mark newer than the version (a reset) sends every version",
            0..1,
            knockSeedVersionsToSend(KnockSeedSentMark(5, devices), 1, devices),
        )
    }

    @Test
    fun `the seeds sent per role change stay constant as admin removals add up`() {
        // Walk 2, defect 6: each removal bumps the version; before, every
        // version went out again on every bump (one in-group message each).
        var mark: KnockSeedSentMark? = null
        val devices = setOf("d1")
        val perBump = mutableListOf<Int>()
        for (version in 0..60) {
            val send = knockSeedVersionsToSend(mark, version, devices)
            if (version > 0) perBump += send.count()
            mark = KnockSeedSentMark(version, devices)
        }
        assertTrue(perBump.all { it == 1 })
    }

    @Test
    fun `the owner sends the knock key, and an admin too after bringing a device back`() {
        assertTrue(knockKeySender(isOwner = true, isAdmin = false, afterReentry = false))
        assertTrue(knockKeySender(isOwner = true, isAdmin = false, afterReentry = true))
        assertFalse("an admin does not re-send on every growth", knockKeySender(false, true, false))
        assertTrue("the admin who re-admitted sends it at once", knockKeySender(false, true, true))
        assertFalse("a plain member never sends it", knockKeySender(false, false, true))
        assertTrue("a re-admission clears the mark, so it re-sends", knockKeyResendDue(mark = null, currentVersion = 2, currentMembers = setOf("a")))
    }

    @Test
    fun `the knock-key envelope is classified by its domain alone`() {
        val domain = "lakko/group-knock-key/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)
        assertTrue(looksLikeKnockKeyEnvelope(domain + ByteArray(35)))
        assertTrue("a malformed one is still dropped, never rendered", looksLikeKnockKeyEnvelope(domain))
        assertFalse(looksLikeKnockKeyEnvelope("{\"v\":1}".toByteArray()))
        assertFalse(looksLikeKnockKeyEnvelope("lakko/group-knock-key/v1".toByteArray()))
    }

    // -----------------------------------------------------------------------
    // The gap and its history
    // -----------------------------------------------------------------------

    private val gapPub = ByteArray(32) { 0x47 }

    private fun owedBundle(epochLeft: ULong = 6uL, gapKey: ByteArray = gapPub) =
        OwedGapBundle("ab", byteArrayOf(1), epochLeft = epochLeft, gapFromMs = 0L, owedSinceMs = 0L, gapKey = gapKey)

    @Test
    fun `the gap bundle is sent once the group moved past the request's epoch, at any later epoch`() {
        val owed = owedBundle()
        assertEquals(OwedBundleStep.WAIT, owedBundleStep(owed, 6uL, 1L))
        assertEquals(OwedBundleStep.WAIT, owedBundleStep(owed, 5uL, 1L))
        assertEquals(OwedBundleStep.SEND, owedBundleStep(owed, 7uL, 1L))
        assertEquals("no epoch secret: a later epoch still sends", OwedBundleStep.SEND, owedBundleStep(owed, 9uL, 1L))
        assertEquals(OwedBundleStep.WAIT, owedBundleStep(owed, null, 1L))
        assertEquals(OwedBundleStep.DROP, owedBundleStep(owed, 7uL, GAP_OWED_TTL_MS + 1L))
        assertEquals("a gap key that is not 32 bytes is never sealed to", OwedBundleStep.DROP, owedBundleStep(owedBundle(gapKey = ByteArray(31)), 7uL, 1L))
        assertEquals(OwedBundleStep.WAIT, owedBundleStep(owedBundle(epochLeft = ULong.MAX_VALUE), ULong.MAX_VALUE, 1L))
    }

    @Test
    fun `an opened gap history is kept only from a signer the joined group accepts`() {
        val signer = ByteArray(32) { 5 }
        var asked: ByteArray? = null
        assertEquals("bundle", acceptedGapHistory(signer, "bundle") { asked = it; true })
        assertArrayEquals(signer, asked)
        assertNull("a plain member's post is dropped", acceptedGapHistory(signer, "bundle") { false })
        assertNull("a failed check drops it", acceptedGapHistory(signer, "bundle") { error("native") })
    }

    @Test
    fun `the old exporter carrier is gone`() {
        val main = java.io.File("src/main/kotlin/me/lakko/app")
        for (file in main.walkTopDown().filter { it.extension == "kt" }) {
            val text = file.readText()
            assertFalse(file.name, text.contains("lakko/conversation-reentry/history/v1"))
            assertFalse(file.name, text.contains("lakko/conversation-reentry/history-rendezvous/v1"))
            assertFalse(file.name, text.contains("REENTRY_HISTORY_EXPORTER_LABEL"))
        }
        val repo = java.io.File("src/main/kotlin/me/lakko/app/chat/ChatRepository.kt").readText()
        assertTrue(repo.contains("sealGapHistoryStructured("))
        assertTrue(repo.contains("openGapHistoryStructured("))
        assertTrue(repo.contains("gapHistorySignerAccepted("))
        assertTrue(repo.contains("gapHistoryStart("))
        assertTrue(repo.contains("gapHistoryRendezvous(bundle.gapKey)"))
    }

    @Test
    fun `a request's gap key is durable before the record, and the record before the send`() {
        val repo = java.io.File("src/main/kotlin/me/lakko/app/chat/ChatRepository.kt").readText()
        val fn = repo.substring(repo.indexOf("private suspend fun requestReentryIfDue("))
        val body = fn.substring(0, fn.indexOf("private fun reentryIntervalMs()"))
        val keySaved = body.indexOf("gapKeyStore.put(")
        val recordSaved = body.indexOf("reentryRecords.put(")
        val composed = body.indexOf("composeConversationReentryEnvelope(")
        assertTrue(keySaved in 0 until recordSaved)
        assertTrue(recordSaved < composed)
        assertTrue("the public half rides the envelope", body.contains("gapKey.publicKey,"))
    }

    @Test
    fun `an owed bundle is kept thirty days, as long as the returning device looks`() {
        assertEquals(30L * 24L * 60L * 60L * 1_000L, GAP_OWED_TTL_MS)
        assertEquals(GAP_HISTORY_LOOK_RETENTION_MS, GAP_OWED_TTL_MS)
        val owed = owedBundle()
        val twoDays = 2L * 24L * 60L * 60L * 1_000L
        assertEquals("past the old one-day bound it still waits", OwedBundleStep.WAIT, owedBundleStep(owed, 6uL, twoDays))
        assertEquals(OwedBundleStep.SEND, owedBundleStep(owed, 7uL, GAP_OWED_TTL_MS))
        assertEquals(OwedBundleStep.DROP, owedBundleStep(owed, 7uL, GAP_OWED_TTL_MS + 1L))
    }

    @Test
    fun `an owed bundle survives its persisted form, the largest epoch included`() {
        for (epoch in listOf(0uL, 6uL, ULong.MAX_VALUE)) {
            val owed = OwedGapBundle("AB", byteArrayOf(1, 2, 3), epochLeft = epoch, gapFromMs = 5L, owedSinceMs = 9L, gapKey = gapPub)
            val back = owed.toRow().toOwed()!!
            assertArrayEquals(gapPub, back.gapKey)
            assertEquals("ab", back.groupIdHex)
            assertArrayEquals(byteArrayOf(1, 2, 3), back.leafSignatureKey)
            assertEquals(epoch, back.epochLeft)
            assertEquals(5L, back.gapFromMs)
            assertEquals(9L, back.owedSinceMs)
        }
        val pubB64 = java.util.Base64.getEncoder().encodeToString(gapPub)
        val broken = OwedGapBundleRow("ab", "not base64!", "6", 0L, 0L, pubB64)
        assertNull(broken.toOwed())
        assertNull(OwedGapBundleRow("ab", "AQ==", "-1", 0L, 0L, pubB64).toOwed())
        assertNull("a gap key that is not 32 bytes", OwedGapBundleRow("ab", "AQ==", "6", 0L, 0L, "AQ==").toOwed())
        assertEquals("ab:cd", owedGapBundleKey("AB", "CD"))
    }

    @Test
    fun `only the owner or an admin re-sends the push routing secret after a re-entry`() {
        assertTrue(reentryResendsPushRoutingSecret(isOwner = true, isAdmin = false))
        assertTrue(reentryResendsPushRoutingSecret(isOwner = false, isAdmin = true))
        assertFalse("a plain member, sibling or other person, does not", reentryResendsPushRoutingSecret(isOwner = false, isAdmin = false))
    }

    // -----------------------------------------------------------------------
    // The returning device's look for its gap bundle
    // -----------------------------------------------------------------------

    private class MemoryGapLookStore : GapLookStore {
        val rows = HashMap<String, HashMap<String, GapLookRecord>>()
        override fun load(accountIdB64: String): Map<String, GapLookRecord> = rows[accountIdB64].orEmpty()
        override fun put(accountIdB64: String, record: GapLookRecord): Boolean {
            rows.getOrPut(accountIdB64) { HashMap() }[record.groupIdHex.lowercase()] = record
            return true
        }
        override fun remove(accountIdB64: String, groupIdHex: String) {
            rows[accountIdB64]?.remove(groupIdHex.lowercase())
        }
        override fun clearForAccount(accountIdB64: String) {
            rows.remove(accountIdB64)
        }
    }

    private val lookSecret = ByteArray(32) { 0x42 }
    private val lookPublic = ByteArray(32) { 0x43 }

    private fun b64(bytes: ByteArray) = java.util.Base64.getEncoder().encodeToString(bytes)

    private fun lookRecord(gapTo: Long = 1_000_000L, keys: List<GapKeyEntry> = listOf(GapKeyEntry(b64(lookPublic), b64(lookSecret)))) = GapLookRecord(
        groupIdHex = "AB",
        keys = keys,
        gapFromMs = 500L,
        gapToMs = gapTo,
        route = ReentryRouteKind.KNOCK,
    )

    /** A stand-in for `gapHistoryRendezvous` (native): the public key tagged. */
    private val fakeRendezvous: (ByteArray) -> ByteArray = { byteArrayOf(0x52) + it }

    @Test
    fun `a look is due at once, then at most once a minute, and dropped after thirty days`() {
        assertEquals(60_000L, GAP_HISTORY_LOOK_INTERVAL_MS)
        assertEquals(30L * 24L * 60L * 60L * 1_000L, GAP_HISTORY_LOOK_RETENTION_MS)
        val fresh = lookRecord(gapTo = 1_000L)
        assertEquals(GapLookStep.LOOK, gapLookStep(fresh, 1_000L))
        val looked = fresh.copy(lastLookMs = 10_000L)
        assertEquals(GapLookStep.WAIT, gapLookStep(looked, 10_000L))
        assertEquals(GapLookStep.WAIT, gapLookStep(looked, 10_000L + GAP_HISTORY_LOOK_INTERVAL_MS - 1L))
        assertEquals(GapLookStep.LOOK, gapLookStep(looked, 10_000L + GAP_HISTORY_LOOK_INTERVAL_MS))
        assertEquals("a clock moved back reads as due", GapLookStep.LOOK, gapLookStep(looked, 5_000L))
        assertEquals(GapLookStep.LOOK, gapLookStep(looked, 1_000L + GAP_HISTORY_LOOK_RETENTION_MS))
        assertEquals(GapLookStep.DROP, gapLookStep(looked, 1_000L + GAP_HISTORY_LOOK_RETENTION_MS + 1L))
    }

    @Test
    fun `the record never prints its secret`() {
        val text = lookRecord().toString()
        assertFalse(text.contains(b64(lookSecret)))
        assertFalse(GapKeyEntry(b64(lookPublic), b64(lookSecret)).toString().contains(b64(lookSecret)))
    }

    @Test
    fun `every held gap key's rendezvous is read, and the bundle is opened with that key's secret`() = kotlinx.coroutines.runBlocking {
        val store = MemoryGapLookStore()
        val second = ByteArray(32) { 0x44 }
        val secondSecret = ByteArray(32) { 0x45 }
        val looker = GapHistoryLooker(store, Clock()::read)
        looker.start("acct", lookRecord(keys = listOf(GapKeyEntry(b64(lookPublic), b64(lookSecret)), GapKeyEntry(b64(second), b64(secondSecret)))))
        val read = ArrayList<ByteArray>()
        var openedWith: ByteArray? = null
        val outcome = looker.lookIfDue(
            "acct",
            "ab",
            read = { r -> read += r; if (r.contentEquals(fakeRendezvous(second))) listOf(byteArrayOf(7)) else emptyList() },
            open = { secret, _, _ -> openedWith = secret.copyOf(); "bundle" },
            rendezvousOf = fakeRendezvous,
        ) { _, _ -> }
        assertEquals(GapLookOutcome.RECEIVED, outcome)
        assertEquals(2, read.size)
        assertArrayEquals(fakeRendezvous(lookPublic), read[0])
        assertArrayEquals(secondSecret, openedWith)
    }

    /** A rendezvous of [rows] rows served in pages the way the DS does:
     *  rows with an id above `after`, at most `limit`, `has_more` when more
     *  remain. Row `i` has id `i + 1`. Records every `(after, limit)` asked. */
    private class PagedRendezvous(val rows: List<ByteArray>) {
        val asked = ArrayList<Pair<Long, Int>>()

        fun page(after: Long, limit: Int): me.lakko.app.ds.FetchResult {
            asked += after to limit
            val rest = rows.withIndex().filter { (it.index + 1L) > after }
            val page = rest.take(limit)
            return me.lakko.app.ds.FetchResult(
                messages = page.map { MailboxMessage(id = it.index + 1L, payload = it.value, createdAt = "t") },
                hasMore = rest.size > limit,
            )
        }
    }

    @Test
    fun `a first page of junk does not hide the bundle on a later page`() = kotlinx.coroutines.runBlocking {
        val bundle = byteArrayOf(0x42, 0x42)
        val junk = List(me.lakko.app.ds.RENDEZVOUS_PAGE_LIMIT) { byteArrayOf(0x11, it.toByte()) }
        // Junk before the bundle and junk after it: newest first, the rows
        // after it are tried first and passed over.
        val rendezvous = PagedRendezvous(junk + listOf(bundle) + List(3) { byteArrayOf(0x22, it.toByte()) })
        val store = MemoryGapLookStore()
        val looker = GapHistoryLooker(store, Clock()::read)
        looker.start("acct", lookRecord())
        var tried = 0
        var got: ByteArray? = null
        val outcome = looker.lookIfDue(
            "acct",
            "ab",
            read = { _ ->
                me.lakko.app.ds.readRendezvousPages { after, limit -> rendezvous.page(after, limit) }.map { it.payload }
            },
            open = { _, payload, _ ->
                tried += 1
                if (payload.contentEquals(bundle)) payload else null
            },
            rendezvousOf = fakeRendezvous,
        ) { _, applied -> got = applied }
        assertEquals(GapLookOutcome.RECEIVED, outcome)
        assertArrayEquals(bundle, got)
        assertEquals("the three rows after the bundle, then the bundle", 4, tried)
        assertEquals(
            listOf(0L to me.lakko.app.ds.RENDEZVOUS_PAGE_LIMIT, 100L to me.lakko.app.ds.RENDEZVOUS_PAGE_LIMIT),
            rendezvous.asked,
        )
        assertNull("the look is dropped once a bundle opened", store.get("acct", "ab"))
    }

    @Test
    fun `the rendezvous read stops after the page cap`() = kotlinx.coroutines.runBlocking {
        assertEquals(100, me.lakko.app.ds.RENDEZVOUS_PAGE_LIMIT)
        assertEquals(10, me.lakko.app.ds.RENDEZVOUS_PAGE_CAP)
        val rendezvous = PagedRendezvous(List(2_500) { byteArrayOf(0x11, (it % 256).toByte()) })
        val rows = me.lakko.app.ds.readRendezvousPages { after, limit -> rendezvous.page(after, limit) }
        assertEquals(me.lakko.app.ds.RENDEZVOUS_PAGE_CAP, rendezvous.asked.size)
        assertEquals(1_000, rows.size)
        assertEquals((0 until 10).map { it * 100L }, rendezvous.asked.map { it.first })
        // A bundle past the cap is not reached: the look keeps waiting.
        val store = MemoryGapLookStore()
        val looker = GapHistoryLooker(store, Clock()::read)
        looker.start("acct", lookRecord())
        val bundle = byteArrayOf(0x42)
        val beyond = PagedRendezvous(List(1_000) { byteArrayOf(0x11) } + listOf(bundle))
        val outcome = looker.lookIfDue(
            "acct",
            "ab",
            read = { _ -> me.lakko.app.ds.readRendezvousPages { a, l -> beyond.page(a, l) }.map { it.payload } },
            open = { _, payload, _ -> if (payload.contentEquals(bundle)) payload else null },
            rendezvousOf = fakeRendezvous,
        ) { _, _ -> }
        assertEquals(GapLookOutcome.NOTHING_YET, outcome)
        assertEquals(10, beyond.asked.size)
    }

    @Test
    fun `the rendezvous read ends on the first page without more`() = kotlinx.coroutines.runBlocking {
        val rendezvous = PagedRendezvous(List(42) { byteArrayOf(it.toByte()) })
        val rows = me.lakko.app.ds.readRendezvousPages { after, limit -> rendezvous.page(after, limit) }
        assertEquals(42, rows.size)
        assertEquals(listOf(0L to 100), rendezvous.asked)
        val empty = PagedRendezvous(emptyList())
        assertTrue(me.lakko.app.ds.readRendezvousPages { a, l -> empty.page(a, l) }.isEmpty())
        assertEquals(1, empty.asked.size)
    }

    @Test
    fun `a look with no gap key is dropped`() = kotlinx.coroutines.runBlocking {
        val store = MemoryGapLookStore()
        val looker = GapHistoryLooker(store, Clock()::read)
        looker.start("acct", lookRecord(keys = emptyList()))
        assertEquals(
            GapLookOutcome.DROPPED,
            looker.lookIfDue<Int>("acct", "ab", read = { emptyList() }, open = { _, _, _ -> 1 }, rendezvousOf = fakeRendezvous) { _, _ -> },
        )
        assertNull(store.get("acct", "ab"))
    }

    @Test
    fun `a failed read, an empty rendezvous or a row that does not open keeps the look`() = kotlinx.coroutines.runBlocking {
        val store = MemoryGapLookStore()
        val clock = Clock()
        val looker = GapHistoryLooker(store, clock::read)
        assertTrue(looker.start("acct", lookRecord()))
        var reads = 0
        var rendezvousSeen: ByteArray? = null

        val failed = looker.lookIfDue<Int>("acct", "ab", read = { r -> reads += 1; rendezvousSeen = r; throw IllegalStateException("offline") }, open = { _, _, _ -> 1 }, rendezvousOf = fakeRendezvous) { _, _ -> }
        assertEquals(GapLookOutcome.NOTHING_YET, failed)
        assertArrayEquals(fakeRendezvous(lookPublic), rendezvousSeen)
        assertEquals(clock.now, store.get("acct", "ab")!!.lastLookMs)

        assertEquals("throttled within the minute", GapLookOutcome.NOT_DUE, looker.lookIfDue<Int>("acct", "ab", read = { reads += 1; emptyList() }, open = { _, _, _ -> 1 }, rendezvousOf = fakeRendezvous) { _, _ -> })
        assertEquals(1, reads)

        clock.now += GAP_HISTORY_LOOK_INTERVAL_MS
        assertEquals(GapLookOutcome.NOTHING_YET, looker.lookIfDue<Int>("acct", "ab", read = { reads += 1; emptyList() }, open = { _, _, _ -> 1 }, rendezvousOf = fakeRendezvous) { _, _ -> })
        clock.now += GAP_HISTORY_LOOK_INTERVAL_MS
        assertEquals(GapLookOutcome.NOTHING_YET, looker.lookIfDue<Int>("acct", "ab", read = { reads += 1; listOf(byteArrayOf(9)) }, open = { _, _, _ -> null }, rendezvousOf = fakeRendezvous) { _, _ -> })
        clock.now += GAP_HISTORY_LOOK_INTERVAL_MS
        assertEquals(GapLookOutcome.NOTHING_YET, looker.lookIfDue<Int>("acct", "ab", read = { reads += 1; listOf(byteArrayOf(9)) }, open = { _, _, _ -> throw IllegalArgumentException("not a bundle") }, rendezvousOf = fakeRendezvous) { _, _ -> })
        assertEquals(4, reads)
        assertTrue("still kept", store.get("acct", "ab") != null)
    }

    @Test
    fun `the first row that opens is applied, rows that are not bundles are passed over, and the look ends`() = kotlinx.coroutines.runBlocking {
        val store = MemoryGapLookStore()
        val looker = GapHistoryLooker(store, Clock()::read)
        looker.start("acct", lookRecord())
        val tried = ArrayList<Int>()
        var receivedRecord: GapLookRecord? = null
        var receivedValue: String? = null
        val outcome = looker.lookIfDue(
            "acct",
            "AB",
            read = { listOf(byteArrayOf(1), byteArrayOf(2), byteArrayOf(3)) },
            open = { secret, payload, route ->
                assertArrayEquals(lookSecret, secret)
                assertEquals(ReentryRouteKind.KNOCK, route)
                tried += payload[0].toInt()
                if (payload[0].toInt() == 2) "bundle" else null
            },
            rendezvousOf = fakeRendezvous,
        ) { record, value ->
            receivedRecord = record
            receivedValue = value
        }
        assertEquals(GapLookOutcome.RECEIVED, outcome)
        assertEquals("newest first, stops at the first that opens", listOf(3, 2), tried)
        assertEquals("bundle", receivedValue)
        assertEquals(500L, receivedRecord!!.gapFromMs)
        assertEquals(1_000_000L, receivedRecord!!.gapToMs)
        assertNull(store.get("acct", "ab"))
        assertEquals(GapLookOutcome.NOT_HELD, looker.lookIfDue<Int>("acct", "ab", read = { emptyList() }, open = { _, _, _ -> 1 }, rendezvousOf = fakeRendezvous) { _, _ -> })
    }

    @Test
    fun `a look past thirty days is dropped without a read`() = kotlinx.coroutines.runBlocking {
        val store = MemoryGapLookStore()
        val clock = Clock()
        val looker = GapHistoryLooker(store, clock::read)
        looker.start("acct", lookRecord(gapTo = clock.now))
        clock.now += GAP_HISTORY_LOOK_RETENTION_MS + 1L
        var reads = 0
        val outcome = looker.lookIfDue<Int>("acct", "ab", read = { reads += 1; emptyList() }, open = { _, _, _ -> 1 }, rendezvousOf = fakeRendezvous) { _, _ -> }
        assertEquals(GapLookOutcome.DROPPED, outcome)
        assertEquals(0, reads)
        assertNull(store.get("acct", "ab"))
    }

    @Test
    fun `one look per conversation at a time`() = kotlinx.coroutines.runBlocking {
        val store = MemoryGapLookStore()
        val looker = GapHistoryLooker(store, Clock()::read)
        looker.start("acct", lookRecord())
        var inner: GapLookOutcome? = null
        val outer = looker.lookIfDue<Int>(
            "acct",
            "ab",
            read = {
                inner = looker.lookIfDue<Int>("acct", "ab", read = { emptyList() }, open = { _, _, _ -> 1 }, rendezvousOf = fakeRendezvous) { _, _ -> }
                emptyList()
            },
            open = { _, _, _ -> 1 },
            rendezvousOf = fakeRendezvous,
        ) { _, _ -> }
        assertEquals(GapLookOutcome.IN_FLIGHT, inner)
        assertEquals(GapLookOutcome.NOTHING_YET, outer)
    }

    @Test
    fun `the gap rows are those from the gap on, newest kept under the cap`() {
        fun m(id: Long) = PersistedMessage(id, "incoming", "t$id", emptyList(), "x")
        val rows = (1L..6L).map(::m)
        val (kept, capped) = gapRows(rows, fromMs = 3L, cap = 10) { it.messageId }
        assertEquals(listOf(3L, 4L, 5L, 6L), kept.map { it.messageId })
        assertFalse(capped)
        val (few, cut) = gapRows(rows, fromMs = 3L, cap = 2) { it.messageId }
        assertEquals(listOf(5L, 6L), few.map { it.messageId })
        assertTrue(cut)
    }

    @Test
    fun `a bundle is incomplete when cut or when the sender holds nothing older than the gap`() {
        fun m(id: Long) = PersistedMessage(id, "incoming", "t$id", emptyList(), "x")
        val held = (1L..6L).map(::m)
        assertFalse(gapBundleIncomplete(held, fromMs = 3L, cut = false) { it.messageId })
        assertTrue(gapBundleIncomplete(held, fromMs = 3L, cut = true) { it.messageId })
        // The sender was away for part of the gap too: no row before it.
        val late = (4L..6L).map(::m)
        assertTrue(gapBundleIncomplete(late, fromMs = 3L, cut = false) { it.messageId })
    }

    @Test
    fun `a bundle that held the whole gap removes the line, a capped one narrows it`() {
        val gap = ReentryGap(fromMs = 100L, toMs = 1_000L)
        assertNull(gapAfterBundle(gap, listOf(150L, 900L), capped = false))
        assertEquals(gap.copy(toMs = 400L), gapAfterBundle(gap, listOf(400L, 900L), capped = true))
        assertNull(gapAfterBundle(gap, emptyList(), capped = false))
        assertEquals(gap, gapAfterBundle(gap, emptyList(), capped = true))
    }

    @Test
    fun `a bundle row's direction follows who sent it`() {
        assertEquals("outgoing", reentryBundleDirection(ReentryRouteKind.OWN_DEVICE, "outgoing"))
        assertEquals("incoming", reentryBundleDirection(ReentryRouteKind.MAILBOX, "outgoing"))
        assertEquals("outgoing", reentryBundleDirection(ReentryRouteKind.MAILBOX, "incoming"))
        assertEquals("incoming", reentryBundleDirection(ReentryRouteKind.KNOCK, "outgoing"))
        assertEquals("system", reentryBundleDirection(ReentryRouteKind.KNOCK, "system"))
    }

    @Test
    fun `gap lines are bounded and an empty range is not a gap`() {
        var gaps = emptyList<ReentryGap>()
        repeat(MAX_REENTRY_GAPS + 3) { i -> gaps = withGap(gaps, ReentryGap(i * 10L, i * 10L + 5)) }
        assertEquals(MAX_REENTRY_GAPS, gaps.size)
        assertEquals(gaps, withGap(gaps, ReentryGap(5L, 5L)))
    }

    @Test
    fun `the gap line is a plain fact`() {
        val line = gapLineText()
        assertEquals("chat.reentry.gap", line.key)
        assertTrue(line.texts.isEmpty())
    }

    // -----------------------------------------------------------------------
    // The apply loop reports what it skipped
    // -----------------------------------------------------------------------

    @Test
    fun `skipped rows are reported with their framed epoch, an own echo is not`() {
        val skipped = mutableListOf<Pair<ULong?, ULong?>>()
        val rows = listOf(
            MailboxMessage(1L, byteArrayOf(7), "a"),
            MailboxMessage(2L, byteArrayOf(9), "b"),
            MailboxMessage(3L, byteArrayOf(5), "c"),
        )
        applyJournalRowsHolding<Unit>(
            groupIdHex = "ab",
            rows = rows,
            after = 0L,
            latestEpoch = 7uL,
            registeredCommits = emptyMap(),
            holds = RegisteredCommitHoldTracker { 0L },
            isOwnEchoRow = { false },
            localEpoch = { 6uL },
            process = { payload -> if (payload[0].toInt() != 5) throw IllegalStateException("fail") },
            framedEpochs = { payload -> payload[0].toULong() to payload[0].toULong() },
            isOwnEchoFailure = { it.message == "own" },
            log = { _, _ -> },
            onSkipped = { _, sealed, local -> skipped += sealed to local },
        ) { _, _ -> }
        assertEquals(listOf<Pair<ULong?, ULong?>>(7uL to 6uL, 9uL to 6uL), skipped)

        skipped.clear()
        applyJournalRowsHolding<Unit>(
            groupIdHex = "ab",
            rows = rows.take(1),
            after = 0L,
            latestEpoch = 7uL,
            registeredCommits = emptyMap(),
            holds = RegisteredCommitHoldTracker { 0L },
            isOwnEchoRow = { false },
            localEpoch = { 6uL },
            process = { throw IllegalStateException("own") },
            framedEpochs = { 6uL to 6uL },
            isOwnEchoFailure = { it.message == "own" },
            log = { _, _ -> },
            onSkipped = { _, sealed, local -> skipped += sealed to local },
        ) { _, _ -> }
        assertTrue(skipped.isEmpty())
    }

    // -----------------------------------------------------------------------
    // Walk 3, W3-R1 — the epoch a request names comes from a fresh probe
    // -----------------------------------------------------------------------

    @Test
    fun `the request names the epoch the probe answered, never another`() = kotlinx.coroutines.runBlocking {
        assertEquals(67uL, probeReentryEpochLeft { 67uL })
        // A probe that names no epoch, or fails, gives nothing to send.
        assertNull(probeReentryEpochLeft { null })
        assertNull(probeReentryEpochLeft { throw java.io.IOException("offline") })
    }

    @Test
    fun `a cancelled probe is not swallowed`() {
        var cancelled = false
        try {
            kotlinx.coroutines.runBlocking {
                probeReentryEpochLeft { throw kotlinx.coroutines.CancellationException("left") }
            }
        } catch (_: kotlinx.coroutines.CancellationException) {
            cancelled = true
        }
        assertTrue(cancelled)
    }

    @Test
    fun `every request probes after the route is resolved and never reads a stored epoch`() {
        val source = java.io.File("src/main/kotlin/me/lakko/app/chat/ChatRepository.kt").readText()
        val body = source.substringAfter("    private suspend fun requestReentryIfDue(groupIdHex: String) {")
            .substringBefore("\n    }\n")
        // No stored epoch anywhere in the request.
        assertFalse("entry.latestEpoch" in body)
        // One probe, unconditional (no "only when reads are refused").
        assertEquals(1, Regex("""dsClient\.knock\(record\.groupId, emptyList\(\)\)""").findAll(body).count())
        assertFalse("readsRefused" in body)
        val probe = body.indexOf("probeReentryEpochLeft")
        val due = body.indexOf("reentryRequestDue.due(")
        val route = body.indexOf("reentryRouteLocked(")
        val knockKey = body.indexOf("knockKeys.get(")
        val mint = body.indexOf("createReentryKeyPackage(")
        val record = body.indexOf("reentryRecords.put(")
        assertTrue(due in 0 until route)
        assertTrue(route in 0 until probe)
        assertTrue(knockKey in 0 until probe)
        assertTrue(probe in 0 until mint)
        assertTrue(mint in 0 until record)
        // A failed probe retries after the short wait and sends nothing.
        val onFail = body.substringAfter("probeReentryEpochLeft").substringBefore("return")
        assertTrue("reentryRetryAfterFailureMs" in onFail)
    }

    // -----------------------------------------------------------------------
    // Walk 3, W3-R2 — automatic sends are held after a refused read
    // -----------------------------------------------------------------------

    @Test
    fun `one refused read holds automatic sends until a read is answered`() {
        val t = tracker()
        assertFalse(t.holdsAutomaticSends("ab"))
        t.noteReadRefused("ab", ownCommitOwed = false, localEpoch = 6uL)
        // Not yet suspected (three refused reads suspect), but held.
        assertFalse(t.stopsActing("ab"))
        assertTrue(t.holdsAutomaticSends("ab"))
        // Another conversation is not touched.
        assertFalse(t.holdsAutomaticSends("cd"))
        // An answered read in step releases them.
        t.observe("ab", ownCommitOwed = false, drained = true, latestEpoch = 6uL, localEpoch = 6uL)
        assertFalse(t.holdsAutomaticSends("ab"))
    }

    @Test
    fun `suspected or confirmed holds automatic sends with no refused read`() {
        assertTrue(automaticSendsHeld(ConversationBehindEntry(level = BehindLevel.SUSPECTED)))
        assertTrue(automaticSendsHeld(ConversationBehindEntry(level = BehindLevel.CONFIRMED)))
        assertTrue(automaticSendsHeld(ConversationBehindEntry(refusedReads = 2)))
        assertFalse(automaticSendsHeld(ConversationBehindEntry()))
        // An answered read that leaves the device suspected keeps them held.
        val t = tracker()
        t.observe("ab", ownCommitOwed = false, drained = true, latestEpoch = 9uL, localEpoch = 6uL)
        assertTrue(t.holdsAutomaticSends("ab"))
    }

    @Test
    fun `every automatic send path asks the held rule, the person's own sends do not`() {
        val source = java.io.File("src/main/kotlin/me/lakko/app/chat/ChatRepository.kt").readText()
        fun body(signature: String): String =
            source.substringAfter(signature).substringBefore("\n    }\n")
        for (sig in listOf(
            "    private suspend fun sendTypingOnMlsThread(",
            "    private suspend fun sendReadReceiptOnMlsThread(",
            "    private suspend fun emitCommitterBeaconIfDueOnMlsThread(",
            "    private suspend fun fanOut(",
        )) {
            assertTrue("$sig is not held after a refused read", "behindHoldsAutomaticSends(groupIdHex)" in body(sig))
        }
        val broadcast = body("    private suspend fun broadcastControlToGroup(")
        assertTrue("automatic: Boolean = true" in broadcast)
        assertTrue(Regex("""if \(automatic\) \{\s*if \(behindHoldsAutomaticSends\(groupIdHex\)\) return null\s*\} else \{\s*requirePersonActionAllowed\(groupIdHex\)""").containsMatchIn(broadcast))
        // The person's own envelopes keep the section 2 rule.
        for (sig in listOf(
            "    public suspend fun setSharedPin(",
            "    public suspend fun setGroupName(",
            "    public suspend fun setGroupPhoto(",
            "    public suspend fun setGroupReactions(",
            "    public suspend fun adoptSignedHandle(",
            "    public suspend fun sendGroupContactRequest(",
        )) {
            assertTrue("$sig is marked automatic", "automatic = false" in source.substringAfter(sig).substringBefore("\n    }\n"))
        }
    }

    // -----------------------------------------------------------------------
    // Walk 3, W3-R5 — the person's own actions
    // -----------------------------------------------------------------------

    @Test
    fun `a person action is attempted after one refused read, held only while behind`() {
        val t = tracker()
        t.noteReadRefused("ab", ownCommitOwed = false, localEpoch = 6uL)
        // Automatic sends wait; the person's own action is attempted.
        assertTrue(t.holdsAutomaticSends("ab"))
        assertFalse(personActionHeld(t.entry("ab")))
        assertFalse(t.stopsActing("ab"))
        // Suspected or confirmed: held.
        assertTrue(personActionHeld(ConversationBehindEntry(level = BehindLevel.SUSPECTED)))
        assertTrue(personActionHeld(ConversationBehindEntry(level = BehindLevel.CONFIRMED)))
        assertFalse(personActionHeld(ConversationBehindEntry(refusedReads = 2)))
        repeat(2) { t.noteReadRefused("ab", ownCommitOwed = false, localEpoch = 6uL) }
        assertTrue(personActionHeld(t.entry("ab")))
    }

    @Test
    fun `a held person action shows that action's own failure message`() {
        val held = personActionHeldError()
        for (key in listOf("chat.error.edit", "chat.error.delete_message", "chat.error.react", "chat.error.pin", "chat.send.failed")) {
            assertEquals(
                me.lakko.app.l10n.UiText(key),
                chatErrorText(held, key, retryMaySucceed = { false }),
            )
        }
    }

    @Test
    fun `every person action is refused while behind before anything is applied locally`() {
        val source = java.io.File("src/main/kotlin/me/lakko/app/chat/ChatRepository.kt").readText()
        fun body(signature: String): String = source.substringAfter(signature).substringBefore("\n    }\n")
        // Edit, delete and reaction run inside the person-action gate.
        for (sig in listOf(
            "    public suspend fun editMessage(",
            "    public suspend fun deleteMessage(",
            "    public suspend fun reactToMessage(",
        )) {
            assertTrue("$sig is not gated", "personAction(groupIdHex) {" in body(sig))
        }
        val person = body("    private suspend fun <T> personAction(")
        assertTrue("requirePersonActionAllowed(groupIdHex)" in person)
        assertTrue("withPublishEvidence(groupIdHex)" in person)
        // Poll, checklist and article ops: refused before the fold.
        val op = body("    public suspend fun sendContentOp(")
        val gate = op.indexOf("requirePersonActionAllowed(groupIdHex)")
        assertTrue(gate >= 0)
        assertTrue(gate < op.indexOf("record.addContentOp("))
        assertTrue(gate < op.indexOf("publishPreparedSend("))
        // The control-message person actions: the send throws before the
        // local apply that follows it.
        for ((sig, apply) in listOf(
            "    public suspend fun setSharedPin(" to "PinStore.saveShared(applicationContext, accountIdB64, groupIdHex",
            "    public suspend fun setGroupName(" to "if (!storeFirst) GroupNameStore.save(",
            "    public suspend fun setGroupPhoto(" to "GroupSettingsStore.savePhoto(",
            "    public suspend fun setGroupReactions(" to "GroupSettingsStore.saveReactions(",
        )) {
            val b = body(sig)
            val send = b.indexOf("automatic = false")
            assertTrue("$sig sends as a person", send >= 0)
            assertTrue("$sig applies before sending", send < b.lastIndexOf(apply))
        }
        // The refusal is a thrown error, never a null a caller could take for sent.
        val required = body("    internal fun requirePersonActionAllowed(")
        assertTrue("throw personActionHeldError()" in required)
    }

    @Test
    fun `back in step is told once, by a read or by the re-entry joining`() {
        val t = tracker()
        val told = mutableListOf<String>()
        t.onBackInStep = { told += it }
        // In step and staying so: nothing.
        t.observe("AB", ownCommitOwed = false, drained = true, latestEpoch = 6uL, localEpoch = 6uL)
        assertTrue(told.isEmpty())
        // Suspected, then a read shows it in step.
        t.observe("AB", ownCommitOwed = false, drained = true, latestEpoch = 7uL, localEpoch = 6uL)
        assertTrue(t.stopsActing("ab"))
        t.observe("AB", ownCommitOwed = false, drained = true, latestEpoch = 7uL, localEpoch = 7uL)
        assertEquals(listOf("ab"), told)
        // The re-entry joined.
        t.cameBack("CD", 5_000L)
        assertEquals(listOf("ab", "cd"), told)
        // A listener that throws does not break the fold.
        t.onBackInStep = { error("boom") }
        t.observe("ef", ownCommitOwed = false, drained = true, latestEpoch = 9uL, localEpoch = 6uL)
        t.observe("ef", ownCommitOwed = false, drained = true, latestEpoch = 9uL, localEpoch = 9uL)
        assertFalse(t.stopsActing("ef"))
    }

    @Test
    fun `a refused-read hold that ends in step is told like back in step (W3-R6)`() {
        val t = tracker()
        val told = mutableListOf<String>()
        t.onBackInStep = { told += it }
        // An answered read with no refusal before it: nothing.
        t.observe("ab", ownCommitOwed = false, drained = true, latestEpoch = 6uL, localEpoch = 6uL)
        assertTrue(told.isEmpty())
        // Refused, then answered in step: told once.
        t.noteReadRefused("ab", ownCommitOwed = false, localEpoch = 6uL)
        assertFalse(t.stopsActing("ab"))
        t.observe("ab", ownCommitOwed = false, drained = true, latestEpoch = 6uL, localEpoch = 6uL)
        assertEquals(listOf("ab"), told)
        t.observe("ab", ownCommitOwed = false, drained = true, latestEpoch = 6uL, localEpoch = 6uL)
        assertEquals(listOf("ab"), told)
        // Refused, then an answered read that suspects: not told.
        t.noteReadRefused("ab", ownCommitOwed = false, localEpoch = 6uL)
        t.observe("ab", ownCommitOwed = false, drained = true, latestEpoch = 8uL, localEpoch = 6uL)
        assertTrue(t.stopsActing("ab"))
        assertEquals(listOf("ab"), told)
    }

    @Test
    fun `back in step re-sends this device's own growth-time state`() {
        val source = java.io.File("src/main/kotlin/me/lakko/app/chat/ChatRepository.kt").readText()
        assertTrue(
            "tracker.onBackInStep = { hex -> pushScope.launchSession { resendOwnGrowthStateBestEffort(hex) } }" in source,
        )
        val body = source.substringAfter("    private suspend fun resendOwnGrowthStateBestEffort(groupIdHex: String) {")
            .substringBefore("\n    }\n")
        for (step in listOf(
            "profileRepublish.publishNow(groupIdHex)",
            "groupNameRepublish.publishNow(groupIdHex)",
            "groupSettingsRepublish.publishNow(groupIdHex)",
            "pinRepublish.onGrowth(groupIdHex)",
            "sharedChannels.groupGrew(groupIdHex)",
            "distributeKnockKeyIfDueBestEffort(groupIdHex, afterReentry = true)",
        )) {
            assertTrue("$step missing", step in body)
        }
    }
}
