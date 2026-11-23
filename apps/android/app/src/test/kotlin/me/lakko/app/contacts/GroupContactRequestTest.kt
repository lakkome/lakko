// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import kotlinx.coroutines.test.runTest
import me.lakko.app.chat.looksLikeGroupContactRequestEnvelope
import me.lakko.app.chat.pickSharedGroup
import me.lakko.app.ui.contacts.viaGroupLine
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/who-can-find-me.md` section 6.4 — a contact request sent inside a
 * group both people are in. Host JVM; the FFI calls are fakes.
 */
class GroupContactRequestTest {

    private val anna = ByteArray(32) { 7 }

    private fun request(
        messageId: Long = 1L,
        from: ByteArray = anna,
        validAfterMs: ULong = 1_700_000_000_000uL,
        via: String? = null,
    ) = IncomingContactRequest(
        messageId = messageId,
        fromPubkey = from,
        fromAccountId = ByteArray(16) { 2 },
        fromPseudonym = ByteArray(32),
        validAfterMs = validAfterMs,
        intro = null,
        createdAt = "2026-11-05T10:00:00Z",
        viaGroupIdHex = via,
    )

    // ---- the send order -------------------------------------------------------

    private class Fake(var waiting: Boolean = false, val reaches: Boolean = false, val shared: String? = null) {
        val founded = mutableListOf<String>()
        val sentIn = mutableListOf<String>()

        suspend fun run(sendOk: Boolean = true): MessageOutcome = runMessageOrder(
            waiting = waiting,
            grantReaches = { reaches },
            sharedGroup = { shared },
            found = { "one2one".also { founded += it } },
            sendInGroup = { group ->
                sentIn += group
                if (sendOk) waiting = true
                sendOk
            },
        )
    }

    @Test
    fun `a grant that reaches them founds the one-to-one`() = runTest {
        val f = Fake(reaches = true, shared = "grp")
        assertEquals(MessageOutcome.Opened("one2one"), f.run())
        assertEquals(listOf("one2one"), f.founded)
        assertTrue(f.sentIn.isEmpty())
    }

    @Test
    fun `with no grant the request goes inside the shared group and no chat is made`() = runTest {
        val f = Fake(reaches = false, shared = "grp")
        assertEquals(MessageOutcome.RequestSent, f.run())
        assertEquals(listOf("grp"), f.sentIn)
        assertTrue(f.founded.isEmpty())
    }

    @Test
    fun `with neither nothing is sent`() = runTest {
        val f = Fake(reaches = false, shared = null)
        assertEquals(MessageOutcome.CannotMessage, f.run())
        assertTrue(f.sentIn.isEmpty())
        assertTrue(f.founded.isEmpty())
    }

    @Test
    fun `Message pressed twice sends once`() = runTest {
        val f = Fake(reaches = false, shared = "grp")
        assertEquals(MessageOutcome.RequestSent, f.run())
        assertEquals(MessageOutcome.RequestSent, f.run())
        assertEquals(listOf("grp"), f.sentIn)
    }

    @Test
    fun `a send that fails is reported and the next press tries again`() = runTest {
        val f = Fake(reaches = false, shared = "grp")
        assertEquals(MessageOutcome.RequestFailed, f.run(sendOk = false))
        assertEquals(MessageOutcome.RequestSent, f.run())
        assertEquals(listOf("grp", "grp"), f.sentIn)
    }

    @Test
    fun `a grant check that fails sends nothing`() = runTest {
        var sent = false
        val outcome = runMessageOrder(
            waiting = false,
            grantReaches = { error("offline") },
            sharedGroup = { "grp" },
            found = { "x" },
            sendInGroup = {
                sent = true
                true
            },
        )
        assertEquals(MessageOutcome.RequestFailed, outcome)
        assertFalse(sent)
    }

    @Test
    fun `a waiting request is read from Sent`() {
        val sent = listOf(SentContactRequest("", "QUJD", 5L, "t"))
        assertTrue(requestWaiting(sent, "QUJD"))
        assertFalse(requestWaiting(sent, "REVG"))
    }

    // ---- which group ----------------------------------------------------------

    @Test
    fun `the shared group is a group both are in, the newest when several`() {
        val people = mapOf(
            "old" to listOf("me", "anna", "boris"),
            "new" to listOf("me", "anna"),
            "one2one" to listOf("me", "anna"),
            "theirs" to listOf("anna", "boris"),
            "gone" to listOf("me", "anna"),
        )
        val kinds = mapOf("old" to true, "new" to true, "one2one" to false, "theirs" to true, "gone" to false)
        val at = mapOf("old" to 100L, "new" to 200L, "theirs" to 900L)
        val pick = { candidates: List<String> ->
            pickSharedGroup(
                candidates = candidates,
                isGroup = { kinds[it] == true },
                people = { people[it] },
                own = "me",
                isPeer = { it == "anna" },
                newestAt = { at[it] },
            )
        }
        assertEquals("new", pick(people.keys.toList()))
        assertEquals("old", pick(listOf("old", "one2one", "theirs")))
        assertNull(pick(listOf("one2one", "theirs", "gone")))
    }

    // ---- receiving ------------------------------------------------------------

    @Test
    fun `a request not for this account is dropped`() {
        var read = false
        val out = openGroupContactRequest(
            envelope = byteArrayOf(1),
            senderKey = anna,
            check = { _, _ -> null },
            toRequest = {
                read = true
                request()
            },
        )
        assertNull(out)
        assertFalse(read)
    }

    @Test
    fun `an opened request comes out as an ordinary request with the group`() {
        val out = openGroupContactRequest(
            envelope = byteArrayOf(1),
            senderKey = anna,
            check = { _, key ->
                assertSame(anna, key)
                byteArrayOf(9)
            },
            toRequest = { request(via = "grp") },
        )
        assertEquals("grp", out?.viaGroupIdHex)
    }

    @Test
    fun `a request from someone other than the member who posted it is dropped`() {
        val out = openGroupContactRequest(
            envelope = byteArrayOf(1),
            senderKey = anna,
            check = { _, _ -> byteArrayOf(9) },
            toRequest = { request(from = ByteArray(32) { 8 }) },
        )
        assertNull(out)
    }

    @Test
    fun `the person's setting decides, unless the sender is a contact`() {
        assertTrue(groupRequestShown(allowsGroup = true, senderIsContact = false))
        assertTrue(groupRequestShown(allowsGroup = false, senderIsContact = true))
        assertFalse(groupRequestShown(allowsGroup = false, senderIsContact = false))
    }

    @Test
    fun `the same request from the mailbox and from a group is one row`() {
        val mailbox = request(messageId = 41L)
        val group = request(messageId = groupRequestMessageId("abcd", 7L), via = "abcd")
        val merged = mergeFetchedRequests(pending = listOf(mailbox), accepted = emptyList(), fetched = listOf(group))
        assertEquals(listOf(mailbox), merged.pending)
        val both = mergeFetchedRequests(pending = emptyList(), accepted = emptyList(), fetched = listOf(group, mailbox))
        assertEquals(1, both.pending.size)
        // A later request of the same person replaces the row: one card per
        // person (2026-11-23).
        val later = request(messageId = 42L, validAfterMs = 1_700_000_000_001uL)
        assertEquals(listOf(42L), mergeFetchedRequests(listOf(mailbox), emptyList(), listOf(later)).pending.map { it.messageId })
    }

    @Test
    fun `a group request id is stable, negative and per row`() {
        val a = groupRequestMessageId("abcd", 7L)
        assertEquals(a, groupRequestMessageId("ABCD", 7L))
        assertTrue(a < 0)
        assertNotEquals(a, groupRequestMessageId("abcd", 8L))
        assertNotEquals(a, groupRequestMessageId("abce", 7L))
    }

    @Test
    fun `the via group survives the pending store`() {
        val decoded = decodePendingRequests(encodePendingRequests(listOf(request(via = "abcd"), request(messageId = 2L))))!!
        assertEquals("abcd", decoded[0].viaGroupIdHex)
        assertNull(decoded[1].viaGroupIdHex)
    }

    @Test
    fun `the Via line names the group, or says it has no name`() {
        assertNull(viaGroupLine(null, "Trip"))
        val named = viaGroupLine("abcd", "Trip")!!
        assertEquals("contacts.request.via_group", named.key)
        assertEquals("Trip", named.groupName)
        assertEquals("contacts.request.via_unnamed_group", viaGroupLine("abcd", null)!!.key)
        assertEquals("contacts.request.via_unnamed_group", viaGroupLine("abcd", " ")!!.key)
    }

    @Test
    fun `a refusal is logged by its code only`() {
        assertEquals(
            "senderMismatch",
            groupRequestErrorCode(IllegalStateException("group contact request senderMismatch: from someone")),
        )
        assertEquals("IllegalStateException", groupRequestErrorCode(IllegalStateException("other")))
    }

    @Test
    fun `the envelope is recognised by its domain and never read as chat`() {
        val env = "lakko/group-contact-request/v1\u0000".toByteArray(Charsets.US_ASCII) + ByteArray(40)
        assertTrue(looksLikeGroupContactRequestEnvelope(env))
        assertFalse(looksLikeGroupContactRequestEnvelope("{\"v\":1}".toByteArray()))
        assertFalse(looksLikeGroupContactRequestEnvelope("lakko/group-contact-request/v1".toByteArray()))
    }

    @Test
    fun `the receive loop makes it its own event, bound to the poster, never a bubble`() {
        val env = "lakko/group-contact-request/v1\u0000".toByteArray(Charsets.US_ASCII) + ByteArray(40) { 1 }
        val e = me.lakko.app.chat.toEvent(
            messageId = 11L,
            createdAt = "2026-11-05T10:00:00Z",
            processed = uniffi.lakko_ffi_uniffi.ProcessedMessage.Application(env, 3u, null),
            resolveSender = { l -> "KEY_$l" },
            resolveLeafSignatureKey = { null },
            isCommitterBeacon = { false },
            parseBody = { error("never parsed as chat") },
            decodeProfile = { null },
            decodeGroupName = { null },
            isProfileRequest = { false },
            decodePin = { null },
            decodeSharedChannels = { null },
        )
        assertTrue("got $e", e is me.lakko.app.chat.IncomingChatEvent.GroupContactRequest)
        e as me.lakko.app.chat.IncomingChatEvent.GroupContactRequest
        assertEquals("KEY_3", e.senderKey)
        assertTrue(e.envelope.contentEquals(env))
    }

    @Test
    fun `a contact with no chat here asking inside a group is a request row`() {
        val contactNoChat = AcceptedContact(request = request(messageId = -5L), groupIdHex = null)
        val viaGroup = request(messageId = groupRequestMessageId("abcd", 3L), via = "abcd")
        val merged = mergeFetchedRequests(emptyList(), listOf(contactNoChat), listOf(viaGroup))
        assertEquals(listOf(viaGroup), merged.pending)
        assertEquals(listOf(viaGroup), withoutAcceptedSenders(merged.pending, listOf(contactNoChat)))
    }

    @Test
    fun `a contact who has a chat here is not asking for one`() {
        val contactWithChat = AcceptedContact(request = request(messageId = -5L), groupIdHex = "one2one")
        val viaGroup = request(messageId = groupRequestMessageId("abcd", 3L), via = "abcd")
        assertTrue(mergeFetchedRequests(emptyList(), listOf(contactWithChat), listOf(viaGroup)).pending.isEmpty())
        // A mailbox request from a contact with a chat here goes as before.
        val mailbox = request(messageId = 9L)
        assertTrue(mergeFetchedRequests(emptyList(), listOf(contactWithChat), listOf(mailbox)).pending.isEmpty())
    }

    @Test
    fun `base scenario 7 - a mailbox request listed because no chat is held here stays a request row`() {
        // The silent answer is given only while a one-to-one is held; without
        // one the known-contact check marks the request and it is listed.
        val plain = request(messageId = 9L)
        val marked = plain.copy(asksForChat = true, asksPastChatHex = "dead")
        val contactStaleChat = AcceptedContact(request = request(messageId = -5L), groupIdHex = "dead")
        assertEquals(listOf(marked), mergeFetchedRequests(emptyList(), listOf(contactStaleChat), listOf(marked)).pending)
        assertEquals(listOf(marked), withoutAcceptedSenders(listOf(marked), listOf(contactStaleChat)))
        // Unmarked, the same row from a contact is not a request (it was
        // answered silently, or re-fetched).
        assertTrue(mergeFetchedRequests(emptyList(), listOf(contactStaleChat), listOf(plain)).pending.isEmpty())
        assertFalse(asksForChat(plain, listOf(contactStaleChat)))
        // Once the row names another chat, they have one: no longer asking.
        val contactNewChat = contactStaleChat.copy(groupIdHex = "fresh")
        assertFalse(asksForChat(marked, listOf(contactNewChat)))
        assertTrue(withoutAcceptedSenders(listOf(marked), listOf(contactNewChat)).isEmpty())
    }
}
