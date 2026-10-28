// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.auth.SealedBlobStorage
import kotlinx.coroutines.runBlocking
import me.lakko.app.auth.base64Encode
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0079 (reworked 2026-10-17) - the claim-then-build rule, the claim
 * record, the sealed store of undecided link requests, and the dead-link
 * note kept with a stored request. Mirror of the web `link-claims.test.ts`
 * and the iOS `LinkClaimsTests`.
 */
class LinkClaimsTest {

    private class MapBlobs : SealedBlobStorage {
        val map = mutableMapOf<String, ByteArray>()
        override fun read(name: String): ByteArray? = map[name]?.copyOf()
        override fun write(name: String, plaintext: ByteArray) {
            map[name] = plaintext.copyOf()
        }
        override fun delete(name: String) {
            map.remove(name)
        }
    }

    private val posted = LinkClaims.Own(ids = listOf(9L))
    private val posting = LinkClaims.Own(wires = listOf("aa"))
    private val won = LinkClaims.Own(ids = listOf(9L), won = 9L)
    private val author = "bb".repeat(32)

    /** Amended 2026-10-24 - a person removed and invited again is put back
     *  with the one-to-one they have; nothing is founded. */
    @Test
    fun `restores a requester who already has a live one-to-one, founding nothing`() {
        val lost = LinkClaims.Own(ids = listOf(9L), settled = true)
        for (verdict in listOf(LinkClaims.Verdict.ACCEPT, LinkClaims.Verdict.ANSWERED)) {
            assertEquals(LinkClaims.Step.RESTORE, LinkClaims.step(null, false, verdict, liveOneToOne = true))
            assertEquals(LinkClaims.Step.RESTORE, LinkClaims.step(lost, true, verdict, liveOneToOne = true))
            assertEquals(LinkClaims.Step.RESTORE, LinkClaims.step(posted, false, verdict, liveOneToOne = true))
        }
        // A dead link is refused as for anyone; an unknown one waits.
        assertEquals(
            LinkClaims.Step.REFUSE,
            LinkClaims.step(null, false, LinkClaims.Verdict.DEAD, liveOneToOne = true),
        )
        assertEquals(
            LinkClaims.Step.WAIT,
            LinkClaims.step(null, false, LinkClaims.Verdict.UNKNOWN, liveOneToOne = true),
        )
        assertEquals(LinkClaims.Step.WAIT, LinkClaims.step(null, false, null, liveOneToOne = true))
        // A claim this device already won keeps building.
        assertEquals(
            LinkClaims.Step.BUILD,
            LinkClaims.step(won, false, LinkClaims.Verdict.ACCEPT, liveOneToOne = true),
        )
        // Without a live one-to-one nothing changes.
        assertEquals(LinkClaims.Step.POST_CLAIM, LinkClaims.step(null, false, LinkClaims.Verdict.ACCEPT))
    }

    @Test
    fun `waits with no timer while the synced state is not loaded`() {
        assertEquals(LinkClaims.Step.WAIT, LinkClaims.step(null, false, null))
    }

    @Test
    fun `follows the register once it is loaded`() {
        assertEquals(LinkClaims.Step.POST_CLAIM, LinkClaims.step(null, false, LinkClaims.Verdict.ACCEPT))
        assertEquals(LinkClaims.Step.DROP, LinkClaims.step(null, false, LinkClaims.Verdict.ANSWERED))
        assertEquals(LinkClaims.Step.REFUSE, LinkClaims.step(null, false, LinkClaims.Verdict.DEAD))
    }

    /** ADR-0079 amendment of 2026-10-19 — a link this device holds no record
     *  of waits, and is never refused. */
    @Test
    fun `waits on a link this device holds no record of`() {
        assertEquals(LinkClaims.Step.WAIT, LinkClaims.step(null, false, LinkClaims.Verdict.UNKNOWN))
    }

    @Test
    fun `waits unshown on a roster device's claim until the register says answered`() {
        assertEquals(LinkClaims.Step.WAIT, LinkClaims.step(null, true, LinkClaims.Verdict.ACCEPT))
        assertEquals(LinkClaims.Step.WAIT, LinkClaims.step(null, true, null))
        assertEquals(LinkClaims.Step.DROP, LinkClaims.step(null, true, LinkClaims.Verdict.ANSWERED))
        assertEquals(LinkClaims.Step.DROP, LinkClaims.step(null, true, LinkClaims.Verdict.DEAD))
        assertEquals(LinkClaims.Step.WAIT, LinkClaims.step(LinkClaims.Own(), true, LinkClaims.Verdict.ACCEPT))
    }

    @Test
    fun `re-checks a lost claim until the request is answered`() {
        val lost = LinkClaims.Own(ids = listOf(9L), settled = true)
        assertEquals(LinkClaims.Step.RECHECK, LinkClaims.step(lost, true, LinkClaims.Verdict.ACCEPT))
        assertEquals(LinkClaims.Step.RECHECK, LinkClaims.step(lost, false, null))
        assertEquals(LinkClaims.Step.DROP, LinkClaims.step(lost, true, LinkClaims.Verdict.ANSWERED))
        assertEquals(LinkClaims.Step.DROP, LinkClaims.step(lost, true, LinkClaims.Verdict.DEAD))
    }

    @Test
    fun `decides by the journal once posted or posting and builds a won claim whatever the verdict says`() {
        assertEquals(LinkClaims.Step.DECIDE, LinkClaims.step(posted, true, LinkClaims.Verdict.ACCEPT))
        assertEquals(LinkClaims.Step.DECIDE, LinkClaims.step(posting, true, null))
        assertEquals(LinkClaims.Step.BUILD, LinkClaims.step(won, true, LinkClaims.Verdict.ANSWERED))
    }

    /** ADR-0079 amendment of 2026-10-19 — a revocation wins until this
     *  account has answered: a won claim not yet founded refuses; once
     *  founded (here, or by another device whose note is held) it builds. */
    @Test
    fun `a revocation before the answer refuses a won claim`() {
        assertEquals(LinkClaims.Step.REFUSE, LinkClaims.step(won, true, LinkClaims.Verdict.DEAD))
        assertEquals(LinkClaims.Step.BUILD, LinkClaims.step(won, true, LinkClaims.Verdict.DEAD, noteHeld = true))
        assertEquals(
            LinkClaims.Step.BUILD,
            LinkClaims.step(won.copy(founded = BuildFake.FOUNDING), true, LinkClaims.Verdict.DEAD),
        )
    }

    @Test
    fun `the request keeps the DS stamp as milliseconds`() {
        assertEquals(1_792_411_200_500L, request(1).copy(createdAt = "2026-10-19T12:00:00.500Z").createdAtMs)
        assertEquals(1_792_411_200_000L, request(1).copy(createdAt = "2026-10-19T12:00:00+00:00").createdAtMs)
        assertNull(request(1).createdAtMs)
    }

    @Test
    fun `the core is handed this device's claims under its own key, then the others`() {
        assertEquals(
            listOf(LinkClaims.Observed(9, "aa".repeat(32)), LinkClaims.Observed(4, author)),
            LinkClaims.claimsToDecide(posted, listOf(LinkClaims.Observed(4, author)), "aa".repeat(32)),
        )
        assertTrue(LinkClaims.claimsToDecide(null, emptyList(), "aa").isEmpty())
    }

    @Test
    fun `the request key is the link id then the requester key, and hex round-trips`() {
        assertEquals(
            "01".repeat(16) + "02".repeat(32),
            LinkClaims.requestKeyHex(ByteArray(16) { 1 }, ByteArray(32) { 2 }),
        )
        assertTrue(LinkClaims.bytes("0aff")!!.contentEquals(byteArrayOf(0x0a, 0xff.toByte())))
        assertNull(LinkClaims.bytes("0"))
        assertNull(LinkClaims.bytes("zz"))
    }

    @Test
    fun `the Welcome goes to the own mailbox before the requester's`() {
        val targets = listOf(
            LinkClaims.FoundedTarget("requester", isRecipient = true),
            LinkClaims.FoundedTarget("own", isRecipient = false),
        )
        assertEquals(listOf("own", "requester"), LinkClaims.welcomeOrder(targets) { it.isRecipient }.map { it.accountKeyB64 })
        assertEquals(1, LinkClaims.welcomeOrder(listOf(targets[0])) { it.isRecipient }.size)
    }

    /** A device for [LinkClaims.runBuild]: recorded state, and every step
     *  logs its name in order. */
    private class BuildFake(
        var founded: LinkClaims.Founded? = null,
        val heldNote: LinkClaims.HeldNote? = null,
        val holdsNotedGroup: Boolean = false,
        val failNote: Boolean = false,
        val noteCounts: Boolean = true,
    ) : LinkClaims.BuildDeps {
        val log = mutableListOf<String>()

        override fun founded(): LinkClaims.Founded? = founded

        override fun note(): LinkClaims.HeldNote? = heldNote

        override suspend fun prepare(): LinkClaims.Founded {
            log += "prepare"
            return FOUNDING
        }

        override fun record(founded: LinkClaims.Founded): Boolean {
            this.founded = founded
            log += "record:${if (founded.notePosted) "note" else "-"}:${if (founded.delivered) "delivered" else "-"}"
            return true
        }

        override suspend fun postNote(founded: LinkClaims.Founded): Boolean {
            if (failNote) throw IllegalStateException("offline")
            log += "postNote"
            return noteCounts
        }

        override suspend fun hold(founded: LinkClaims.Founded): Boolean {
            log += "hold"
            return true
        }

        override suspend fun deliver(founded: LinkClaims.Founded) {
            log += "deliver"
        }

        override suspend fun forward(note: LinkClaims.HeldNote): String? {
            log += "forward:${note.groupIdHex}"
            return note.groupIdHex.takeIf { holdsNotedGroup }
        }

        override suspend fun finish(groupIdHex: String?) {
            log += "finish:${groupIdHex ?: "none"}"
        }

        companion object {
            val FOUNDING = LinkClaims.Founded(
                groupIdHex = "aa",
                welcomeB64 = "w",
                targets = listOf(
                    LinkClaims.FoundedTarget("own", isRecipient = false),
                    LinkClaims.FoundedTarget("requester", isRecipient = true),
                ),
                notePosted = false,
                delivered = false,
            )
        }
    }

    @Test
    fun `a takeover device not in the founded group that sees a note forwards and never founds`() = runBlocking {
        val device = BuildFake(heldNote = LinkClaims.HeldNote(7, "theirs", "w"), holdsNotedGroup = false)
        assertTrue(LinkClaims.runBuild(device))
        assertEquals(listOf("forward:theirs", "finish:none"), device.log)
        assertEquals(LinkClaims.BuildStep.FORWARD, LinkClaims.buildStep(hasFounded = false, hasNote = true))
    }

    @Test
    fun `the note is posted before any Welcome and each step is recorded first`() = runBlocking {
        val device = BuildFake()
        assertTrue(LinkClaims.runBuild(device))
        assertEquals(
            listOf("prepare", "record:-:-", "postNote", "record:note:-", "hold", "deliver", "record:note:delivered", "finish:aa"),
            device.log,
        )
    }

    @Test
    fun `no Welcome leaves when the note's publish fails`() = runBlocking {
        val device = BuildFake(failNote = true)
        assertTrue(runCatching { LinkClaims.runBuild(device) }.isFailure)
        assertFalse(device.log.contains("deliver"))
        assertEquals(false, device.founded?.notePosted)
    }

    @Test
    fun `a recorded founding is resumed, never founded again, even with a note held`() = runBlocking {
        val device = BuildFake(
            founded = BuildFake.FOUNDING.copy(notePosted = true),
            heldNote = LinkClaims.HeldNote(7, "theirs", "w"),
        )
        assertTrue(LinkClaims.runBuild(device))
        assertFalse(device.log.contains("prepare"))
        assertFalse(device.log.contains("postNote"))
        assertFalse(device.log.contains("forward:theirs"))
        assertEquals(LinkClaims.BuildStep.RESUME, LinkClaims.buildStep(hasFounded = true, hasNote = true))
        assertEquals(LinkClaims.BuildStep.FOUND, LinkClaims.buildStep(hasFounded = false, hasNote = false))
    }

    @Test
    fun `the record keeps the earliest note per request, bounded, across instances`() {
        val storage = InMemoryLinkClaimStorage()
        val record = LinkClaimRecord("acct", storage)
        assertNull(record.note("k"))
        assertTrue(record.recordNote("k", LinkClaims.HeldNote(9, "a", "w"), 1))
        assertFalse(record.recordNote("k", LinkClaims.HeldNote(12, "b", "w"), 2))
        assertTrue(record.recordNote("k", LinkClaims.HeldNote(4, "c", "w"), 3))
        assertEquals("c", LinkClaimRecord("acct", storage).note("k")?.groupIdHex)
        for (i in 0..LinkClaimRecord.MAX_NOTES) record.recordNote("k$i", LinkClaims.HeldNote(1, "g", "w"), (i + 10).toLong())
        assertNull(record.note("k"))
    }

    @Test
    fun `the founding is recorded with its note flag`() {
        val storage = InMemoryLinkClaimStorage()
        assertTrue(LinkClaimRecord("acct", storage).setFounded("k", BuildFake.FOUNDING))
        assertEquals(BuildFake.FOUNDING, LinkClaimRecord("acct", storage).own("k")?.founded)
    }

    @Test
    fun `the record keeps every seen claim with its author, per account`() {
        val storage = InMemoryLinkClaimStorage()
        val record = LinkClaimRecord("acct", storage)
        assertTrue(record.seen("k").isEmpty())
        assertTrue(record.recordSeen("k", 12, author, 1))
        assertFalse(record.recordSeen("k", 12, author, 2))
        assertTrue(record.recordSeen("k", 9, author, 3))
        assertEquals(listOf(9L, 12L), LinkClaimRecord("acct", storage).seen("k").map { it.id })
        assertTrue(LinkClaimRecord("other", storage).seen("k").isEmpty())
    }

    @Test
    fun `the own claim row is recognised by its bytes after a lost publish answer`() {
        val storage = InMemoryLinkClaimStorage()
        val record = LinkClaimRecord("acct", storage)
        assertTrue(record.addOwnWire("k", "c0ffee"))
        assertTrue(record.hasUnmatchedWires())
        val later = LinkClaimRecord("acct", storage)
        assertNull(later.judgeOwnRow("beef", 10, counts = true))
        assertEquals("claim", later.judgeOwnRow("c0ffee", 10, counts = true))
        assertEquals(LinkClaims.Own(ids = listOf(10L)), record.own("k"))
        assertFalse(record.hasUnmatchedWires())
        record.addOwnWire("k", "d00d")
        record.judgeOwnRow("d00d", 14, counts = true)
        assertEquals(listOf(10L, 14L), record.own("k")?.ids)
        record.addOwnWire("k", "aa")
        record.dropUnlanded("k")
        assertEquals(emptyList<String>(), record.own("k")?.wires)
        assertTrue(record.setSettled("k"))
        assertEquals(true, record.own("k")?.settled)
        assertTrue(record.setWon("k", 10))
        assertEquals(10L, LinkClaimRecord("acct", storage).own("k")?.won)
        record.forgetOwn("k")
        assertNull(record.own("k"))
    }

    @Test
    fun `a straddled own claim does not count for its author either`() {
        val record = LinkClaimRecord("acct", InMemoryLinkClaimStorage())
        record.addOwnWire("k", "c0ffee")
        assertEquals("claim", record.judgeOwnRow("c0ffee", 10, counts = false))
        assertEquals(emptyList<Long>(), record.own("k")?.ids)
        assertEquals(emptyList<String>(), record.own("k")?.wires)
        assertEquals(
            LinkClaims.Step.POST_CLAIM,
            LinkClaims.step(record.own("k"), false, LinkClaims.Verdict.ACCEPT),
        )
    }

    @Test
    fun `a note is confirmed only when its row counts`() {
        val storage = InMemoryLinkClaimStorage()
        val record = LinkClaimRecord("acct", storage)
        assertTrue(record.setNoteWire("k", "abcd"))
        assertTrue(record.hasUnmatchedWires())
        assertEquals("note", record.judgeOwnRow("abcd", 20, counts = false))
        assertEquals(false, record.own("k")?.noteConfirmed)
        assertNull(record.own("k")?.noteWire)
        record.setNoteWire("k", "ef01")
        assertEquals("note", LinkClaimRecord("acct", storage).judgeOwnRow("ef01", 22, counts = true))
        assertEquals(true, record.own("k")?.noteConfirmed)
        record.setNoteWire("k", "aa")
        record.dropUnlandedNote("k")
        assertNull(record.own("k")?.noteWire)
        assertFalse(record.hasUnmatchedWires())
    }

    @Test
    fun `no Welcome is delivered on a straddled note`() = runBlocking {
        val device = BuildFake(noteCounts = false)
        assertFalse(LinkClaims.runBuild(device))
        assertEquals(listOf("prepare", "record:-:-", "postNote"), device.log)
        assertEquals(false, device.founded?.notePosted)
    }

    @Test
    fun `the journal record is kept per DeviceGroup`() {
        val storage = InMemoryLinkClaimStorage()
        val record = LinkClaimRecord("acct", storage)
        assertNull(record.journal("aa"))
        assertTrue(record.setJournal(StoredJournal("aa", 5, listOf(21L), listOf(6L))))
        assertEquals(listOf(21L), LinkClaimRecord("acct", storage).journal("aa")?.commitRowIds)
        assertNull(record.journal("bb"))
    }

    @Test
    fun `the record bounds the claims per request and the requests, dropping the oldest`() {
        val record = LinkClaimRecord("acct", InMemoryLinkClaimStorage())
        for (i in LinkClaims.MAX_CLAIMS_PER_REQUEST + 5 downTo 1) record.recordSeen("r", i.toLong(), author, 1)
        assertEquals(LinkClaims.MAX_CLAIMS_PER_REQUEST, record.seen("r").size)
        assertEquals(1L, record.seen("r").first().id)
        for (i in 0..LinkClaims.MAX_SEEN) record.recordSeen("k$i", (i + 1).toLong(), author, i.toLong() + 2)
        assertTrue(record.seen("r").isEmpty())
        assertEquals(1, record.seen("k${LinkClaims.MAX_SEEN}").size)
    }

    private fun request(id: Long) = IncomingContactRequest(
        messageId = id,
        fromPubkey = ByteArray(32) { 3 },
        fromAccountId = ByteArray(16) { 4 },
        fromPseudonym = ByteArray(32),
        validAfterMs = 0uL,
        intro = null,
        createdAt = "t",
    )

    @Test
    fun `an undecided request survives a restart sealed, and goes once decided`() {
        val blobs = MapBlobs()
        val account = base64Encode(ByteArray(16) { 1 })
        val inbox = LinkRequestInbox(account, blobs)
        assertTrue(inbox.add(UndecidedLinkRequest(request(5), ByteArray(16) { 1 }, ByteArray(12) { 2 })))
        assertTrue(inbox.add(UndecidedLinkRequest(request(5), ByteArray(16) { 1 }, ByteArray(12) { 2 })))
        val again = LinkRequestInbox(account, blobs)
        assertEquals(listOf(5L), again.rows().map { it.request.messageId })
        assertTrue(again.rows()[0].linkSecret.contentEquals(ByteArray(12) { 2 }))
        again.remove(5)
        assertTrue(inbox.rows().isEmpty())
        assertTrue(blobs.map.isEmpty())
    }

    @Test
    fun `an undecided request keeps its DS stamp across a restart`() {
        val blobs = MapBlobs()
        val account = base64Encode(ByteArray(16) { 1 })
        val stamped = request(6).copy(createdAt = "2026-10-19T12:00:00Z")
        assertTrue(LinkRequestInbox(account, blobs).add(UndecidedLinkRequest(stamped, ByteArray(16) { 1 }, ByteArray(12) { 2 })))
        val back = LinkRequestInbox(account, blobs).rows().single().request
        assertEquals("2026-10-19T12:00:00Z", back.createdAt)
        assertEquals(stamped.createdAtMs, back.createdAtMs)
    }
}
