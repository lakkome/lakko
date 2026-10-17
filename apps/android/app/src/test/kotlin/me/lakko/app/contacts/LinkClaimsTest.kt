// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.auth.SealedBlobStorage
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

    @Test
    fun `waits with no timer while the synced state is not loaded`() {
        assertEquals(LinkClaims.Step.WAIT, LinkClaims.step(null, false, null))
    }

    @Test
    fun `follows the register once it is loaded`() {
        assertEquals(LinkClaims.Step.POST_CLAIM, LinkClaims.step(null, false, LinkClaims.Verdict.ACCEPT))
        assertEquals(LinkClaims.Step.DROP, LinkClaims.step(null, false, LinkClaims.Verdict.ANSWERED))
        assertEquals(LinkClaims.Step.CARD_DEAD, LinkClaims.step(null, false, LinkClaims.Verdict.DEAD))
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
        assertEquals(LinkClaims.Step.BUILD, LinkClaims.step(won, true, LinkClaims.Verdict.DEAD))
        assertEquals(LinkClaims.Step.BUILD, LinkClaims.step(won, true, LinkClaims.Verdict.ANSWERED))
    }

    @Test
    fun `a dead-link card goes only for another device's claim`() {
        assertTrue(LinkClaims.shownCardAnswered(null, true))
        assertFalse(LinkClaims.shownCardAnswered(null, false))
        assertFalse(LinkClaims.shownCardAnswered(won, true))
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

    @Test
    fun `a takeover device that already holds a one-to-one with the requester does not build`() {
        val requester = "ab".repeat(32)
        val held = listOf(
            LinkClaims.HeldConversation(oneToOne = false, memberAccountKeysHex = listOf(requester, "cd".repeat(32))),
            LinkClaims.HeldConversation(oneToOne = true, memberAccountKeysHex = listOf("ef".repeat(32), requester.uppercase())),
        )
        assertTrue(LinkClaims.holdsOneToOneWith(held, requester))
        assertEquals(LinkClaims.BuildStep.ANSWERED, LinkClaims.buildStep(false, mailboxCaughtUp = true, holdsOneToOne = true))
        // A group (not a one-to-one) with the requester does not answer it.
        assertFalse(LinkClaims.holdsOneToOneWith(listOf(held[0]), requester))
        assertEquals(LinkClaims.BuildStep.FOUND, LinkClaims.buildStep(false, mailboxCaughtUp = true, holdsOneToOne = false))
    }

    @Test
    fun `a takeover forwards the Welcome it joined from, kept per one-to-one and bounded`() {
        val record = LinkClaimRecord("acct", InMemoryLinkClaimStorage())
        assertNull(record.joinedWelcome("g"))
        assertTrue(record.recordJoinedWelcome("g", "w", 1))
        assertEquals("w", record.joinedWelcome("g"))
        for (i in 0..LinkClaimRecord.MAX_JOINED_WELCOMES) record.recordJoinedWelcome("g$i", "w$i", (i + 2).toLong())
        assertNull(record.joinedWelcome("g"))
        assertEquals("w${LinkClaimRecord.MAX_JOINED_WELCOMES}", record.joinedWelcome("g${LinkClaimRecord.MAX_JOINED_WELCOMES}"))
    }

    @Test
    fun `never builds blind and never founds twice`() {
        assertEquals(LinkClaims.BuildStep.WAIT, LinkClaims.buildStep(false, mailboxCaughtUp = false, holdsOneToOne = false))
        assertEquals(LinkClaims.BuildStep.WAIT, LinkClaims.buildStep(false, mailboxCaughtUp = false, holdsOneToOne = true))
        assertEquals(LinkClaims.BuildStep.RESUME, LinkClaims.buildStep(true, mailboxCaughtUp = false, holdsOneToOne = true))
        val storage = InMemoryLinkClaimStorage()
        val record = LinkClaimRecord("acct", storage)
        val founded = LinkClaims.Founded(
            groupIdHex = "aa",
            welcomeB64 = "w",
            targets = listOf(
                LinkClaims.FoundedTarget("own", isRecipient = false),
                LinkClaims.FoundedTarget("requester", isRecipient = true),
            ),
            delivered = false,
        )
        assertTrue(record.setWon("k", 3))
        assertTrue(record.setFounded("k", founded))
        assertEquals(founded, LinkClaimRecord("acct", storage).own("k")?.founded)
        record.setFounded("k", founded.copy(welcomeB64 = "", delivered = true))
        assertEquals(true, LinkClaimRecord("acct", storage).own("k")?.founded?.delivered)
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
        assertFalse(later.recordOwnClaimRow("beef", 10))
        assertTrue(later.recordOwnClaimRow("c0ffee", 10))
        assertEquals(LinkClaims.Own(ids = listOf(10L)), record.own("k"))
        assertFalse(record.hasUnmatchedWires())
        record.addOwnWire("k", "d00d")
        record.recordOwnClaimRow("d00d", 14)
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
    fun `the dead-link note is kept with the stored request`() {
        val card = request(6).copy(viaDeadLink = true, deadLinkId = ByteArray(16) { 1 })
        val back = decodePendingRequests(encodePendingRequests(listOf(card)))!!.single()
        assertTrue(back.viaDeadLink)
        assertTrue(back.deadLinkId!!.contentEquals(ByteArray(16) { 1 }))
        assertFalse(decodePendingRequests(encodePendingRequests(listOf(request(7))))!!.single().viaDeadLink)
    }
}
