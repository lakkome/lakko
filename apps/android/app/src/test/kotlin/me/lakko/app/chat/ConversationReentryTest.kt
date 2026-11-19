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
        assertTrue("the waiting note is a durable flag", again.isConfirmed("ab"))
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
    fun `cleared on landing forgets the entry and persists it`() {
        val store = MemoryBehindStore()
        val t = tracker(store)
        t.noteSkipped("ab", 7uL, 6uL, 10L)
        t.observe("ab", false, true, 7uL, 6uL)
        t.cleared("AB")
        assertFalse(t.stopsActing("ab"))
        assertTrue(store.saved["acct"].orEmpty().isEmpty())
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
    fun `the owner re-sends the knock key on a new version and on growth`() {
        assertTrue(knockKeyResendDue(null, 0, false))
        assertFalse(knockKeyResendDue(0, 0, false))
        assertTrue(knockKeyResendDue(0, 1, false))
        assertTrue(knockKeyResendDue(1, 1, true))
    }

    @Test
    fun `the owner sends the knock key, and an admin too after bringing a device back`() {
        assertTrue(knockKeySender(isOwner = true, isAdmin = false, afterReentry = false))
        assertTrue(knockKeySender(isOwner = true, isAdmin = false, afterReentry = true))
        assertFalse("an admin does not re-send on every growth", knockKeySender(false, true, false))
        assertTrue("the admin who re-admitted sends it at once", knockKeySender(false, true, true))
        assertFalse("a plain member never sends it", knockKeySender(false, false, true))
        assertTrue("a re-admission always re-sends", knockKeyResendDue(sentVersion = 2, currentVersion = 2, grew = true))
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

    @Test
    fun `the gap bundle is sent at exactly the join epoch`() {
        val owed = OwedGapBundle("ab", byteArrayOf(1), epochLeft = 6uL, gapFromMs = 0L, owedSinceMs = 0L)
        assertEquals(OwedBundleStep.WAIT, owedBundleStep(owed, 6uL, 1L))
        assertEquals(OwedBundleStep.SEND, owedBundleStep(owed, 7uL, 1L))
        assertEquals(OwedBundleStep.DROP, owedBundleStep(owed, 8uL, 1L))
        assertEquals(OwedBundleStep.WAIT, owedBundleStep(owed, null, 1L))
        assertEquals(OwedBundleStep.DROP, owedBundleStep(owed, 6uL, GAP_OWED_TTL_MS + 1L))
    }

    @Test
    fun `an owed bundle is kept thirty days, as long as the returning device looks`() {
        assertEquals(30L * 24L * 60L * 60L * 1_000L, GAP_OWED_TTL_MS)
        assertEquals(GAP_HISTORY_LOOK_RETENTION_MS, GAP_OWED_TTL_MS)
        val owed = OwedGapBundle("ab", byteArrayOf(1), epochLeft = 6uL, gapFromMs = 0L, owedSinceMs = 0L)
        val twoDays = 2L * 24L * 60L * 60L * 1_000L
        assertEquals("past the old one-day bound it still waits", OwedBundleStep.WAIT, owedBundleStep(owed, 6uL, twoDays))
        assertEquals(OwedBundleStep.SEND, owedBundleStep(owed, 7uL, GAP_OWED_TTL_MS))
        assertEquals(OwedBundleStep.DROP, owedBundleStep(owed, 7uL, GAP_OWED_TTL_MS + 1L))
    }

    @Test
    fun `an owed bundle survives its persisted form, the largest epoch included`() {
        for (epoch in listOf(0uL, 6uL, ULong.MAX_VALUE)) {
            val owed = OwedGapBundle("AB", byteArrayOf(1, 2, 3), epochLeft = epoch, gapFromMs = 5L, owedSinceMs = 9L)
            val back = owed.toRow().toOwed()!!
            assertEquals("ab", back.groupIdHex)
            assertArrayEquals(byteArrayOf(1, 2, 3), back.leafSignatureKey)
            assertEquals(epoch, back.epochLeft)
            assertEquals(5L, back.gapFromMs)
            assertEquals(9L, back.owedSinceMs)
        }
        val broken = OwedGapBundleRow("ab", "not base64!", "6", 0L, 0L)
        assertNull(broken.toOwed())
        assertNull(OwedGapBundleRow("ab", "AQ==", "-1", 0L, 0L).toOwed())
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

    private fun lookRecord(gapTo: Long = 1_000_000L) = GapLookRecord(
        groupIdHex = "AB",
        secretB64 = java.util.Base64.getEncoder().encodeToString(lookSecret),
        gapFromMs = 500L,
        gapToMs = gapTo,
        route = ReentryRouteKind.KNOCK,
    )

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
        assertFalse(text.contains(lookRecord().secretB64))
    }

    @Test
    fun `a failed read, an empty rendezvous or a row that does not open keeps the look`() = kotlinx.coroutines.runBlocking {
        val store = MemoryGapLookStore()
        val clock = Clock()
        val looker = GapHistoryLooker(store, clock::read)
        assertTrue(looker.start("acct", lookRecord()))
        var reads = 0
        var rendezvousSeen: ByteArray? = null

        val failed = looker.lookIfDue<Int>("acct", "ab", read = { r -> reads += 1; rendezvousSeen = r; throw IllegalStateException("offline") }, open = { _, _, _ -> 1 }) { _, _ -> }
        assertEquals(GapLookOutcome.NOTHING_YET, failed)
        assertArrayEquals(reentryHistoryRendezvous(lookSecret), rendezvousSeen)
        assertEquals(clock.now, store.get("acct", "ab")!!.lastLookMs)

        assertEquals("throttled within the minute", GapLookOutcome.NOT_DUE, looker.lookIfDue<Int>("acct", "ab", read = { reads += 1; emptyList() }, open = { _, _, _ -> 1 }) { _, _ -> })
        assertEquals(1, reads)

        clock.now += GAP_HISTORY_LOOK_INTERVAL_MS
        assertEquals(GapLookOutcome.NOTHING_YET, looker.lookIfDue<Int>("acct", "ab", read = { reads += 1; emptyList() }, open = { _, _, _ -> 1 }) { _, _ -> })
        clock.now += GAP_HISTORY_LOOK_INTERVAL_MS
        assertEquals(GapLookOutcome.NOTHING_YET, looker.lookIfDue<Int>("acct", "ab", read = { reads += 1; listOf(byteArrayOf(9)) }, open = { _, _, _ -> null }) { _, _ -> })
        clock.now += GAP_HISTORY_LOOK_INTERVAL_MS
        assertEquals(GapLookOutcome.NOTHING_YET, looker.lookIfDue<Int>("acct", "ab", read = { reads += 1; listOf(byteArrayOf(9)) }, open = { _, _, _ -> throw IllegalArgumentException("not a bundle") }) { _, _ -> })
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
        assertEquals(GapLookOutcome.NOT_HELD, looker.lookIfDue<Int>("acct", "ab", read = { emptyList() }, open = { _, _, _ -> 1 }) { _, _ -> })
    }

    @Test
    fun `a look past thirty days is dropped without a read`() = kotlinx.coroutines.runBlocking {
        val store = MemoryGapLookStore()
        val clock = Clock()
        val looker = GapHistoryLooker(store, clock::read)
        looker.start("acct", lookRecord(gapTo = clock.now))
        clock.now += GAP_HISTORY_LOOK_RETENTION_MS + 1L
        var reads = 0
        val outcome = looker.lookIfDue<Int>("acct", "ab", read = { reads += 1; emptyList() }, open = { _, _, _ -> 1 }) { _, _ -> }
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
                inner = looker.lookIfDue<Int>("acct", "ab", read = { emptyList() }, open = { _, _, _ -> 1 }) { _, _ -> }
                emptyList()
            },
            open = { _, _, _ -> 1 },
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
    fun `golden - the rendezvous id is SHA-256 over the label and the secret`() {
        // The cross-client vector (web, iOS and Android agree on it).
        val id = reentryHistoryRendezvous(ByteArray(32) { 0x42 })
        assertEquals(
            "8b4c89a03f20c038dfe71cf88eeee780a4be0af619b852cedbc63ad0dc6eb7dd",
            id.joinToString("") { "%02x".format(it) },
        )
        assertEquals("lakko/conversation-reentry/history/v1", REENTRY_HISTORY_EXPORTER_LABEL)
    }

    @Test
    fun `another secret names another rendezvous`() {
        val a = reentryHistoryRendezvous(ByteArray(32) { 1 })
        assertEquals(32, a.size)
        assertArrayEquals(a, reentryHistoryRendezvous(ByteArray(32) { 1 }))
        assertFalse(a.contentEquals(reentryHistoryRendezvous(ByteArray(32) { 2 })))
    }

    @Test
    fun `gap lines are bounded and an empty range is not a gap`() {
        var gaps = emptyList<ReentryGap>()
        repeat(MAX_REENTRY_GAPS + 3) { i -> gaps = withGap(gaps, ReentryGap(i * 10L, i * 10L + 5)) }
        assertEquals(MAX_REENTRY_GAPS, gaps.size)
        assertEquals(gaps, withGap(gaps, ReentryGap(5L, 5L)))
    }

    @Test
    fun `the gap line shows the date once on the same day`() {
        val day: (Long) -> Long = { it / 1_000L }
        val sameDay = gapLineText(ReentryGap(1_100L, 1_900L), day, { "D${it / 1_000}" }, { "T$it" })
        assertEquals("chat.reentry.gap", sameDay.key)
        assertEquals(mapOf("from" to "D1, T1100", "to" to "T1900"), sameDay.texts)
        val twoDays = gapLineText(ReentryGap(1_100L, 2_200L), day, { "D${it / 1_000}" }, { "T$it" })
        assertEquals(mapOf("from" to "D1, T1100", "to" to "D2, T2200"), twoDays.texts)
    }

    // -----------------------------------------------------------------------
    // The waiting note
    // -----------------------------------------------------------------------

    @Test
    fun `no second line in the first thirty seconds, then the one that names the helper`() {
        assertNull(waitingSecondLine(ReentryHelper.OWN_DEVICE, 0L, REENTRY_SECOND_LINE_AFTER_MS - 1, null))
        assertEquals(
            "chat.reentry.second.own_device",
            waitingSecondLine(ReentryHelper.OWN_DEVICE, 0L, REENTRY_SECOND_LINE_AFTER_MS, null)?.key,
        )
        val person = waitingSecondLine(ReentryHelper.OTHER_PERSON, 0L, REENTRY_SECOND_LINE_AFTER_MS, "Ana")
        assertEquals("chat.reentry.second.person", person?.key)
        assertEquals(mapOf("name" to "Ana"), person?.texts)
        assertNull("no name, no sentence", waitingSecondLine(ReentryHelper.OTHER_PERSON, 0L, 60_000L, " "))
        assertEquals(
            "chat.reentry.second.group",
            waitingSecondLine(ReentryHelper.OWNER_OR_ADMIN, 0L, 60_000L, null)?.key,
        )
        assertEquals(ReentryHelper.OTHER_PERSON, helperOf(ReentryRouteKind.MAILBOX))
        assertEquals(ReentryHelper.OWN_DEVICE, helperOf(ReentryRouteKind.OWN_DEVICE))
        assertEquals(ReentryHelper.OWNER_OR_ADMIN, helperOf(ReentryRouteKind.KNOCK))
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
}
