// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.ChecklistItemRef
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.MessageContent
import uniffi.lakko_ffi_uniffi.ParsedBody
import uniffi.lakko_ffi_uniffi.ProcessedMessage
import uniffi.lakko_ffi_uniffi.SharedLocation

/**
 * ADR-0092 on the Android host JVM: the receive classifier routes content
 * kinds to a row and ops to the content-op event; the ballot schedule waits a
 * uniform 0..=10 s; the content summary keys; the op kind mapping.
 */
class AttachmentKindsTest {

    private val location = MessageContent.Location(SharedLocation(523702200, 48951700, 12u, 1L, "Cafe"))

    private fun body(kind: MessageBodyKind, content: MessageContent? = null, target: Long? = null): ParsedBody = ParsedBody(
        kind = kind,
        text = "",
        attachments = emptyList(),
        target = target,
        emoji = null,
        isTyping = null,
        replyTo = null,
        replyAuthor = null,
        replyExcerpt = null,
        forwardedFrom = null,
        forwardOriginGroupB64 = null,
        forwardOriginMessageId = null,
        content = content,
        pollChoices = if (kind == MessageBodyKind.POLL_VOTE) byteArrayOf(1) else null,
        checklistItem = if (kind == MessageBodyKind.CHECKLIST_TICK) ChecklistItemRef.Author(2u) else null,
        checklistDone = if (kind == MessageBodyKind.CHECKLIST_TICK) true else null,
    )

    private fun receive(parsed: ParsedBody): IncomingChatEvent = toEvent(
        messageId = 7L,
        createdAt = "2026-11-15T00:00:00Z",
        processed = ProcessedMessage.Application(ByteArray(0), 0u, null),
        resolveSender = { "acct" },
        resolveLeafSignatureKey = { "dev" },
        isCommitterBeacon = { false },
        parseBody = { parsed },
    )

    @Test
    fun `a content kind is a timeline row carrying its content`() {
        for (kind in listOf(
            MessageBodyKind.POLL,
            MessageBodyKind.CHECKLIST,
            MessageBodyKind.LOCATION,
            MessageBodyKind.CONTACT_CARD,
            MessageBodyKind.ARTICLE,
        )) {
            val event = receive(body(kind, location))
            assertTrue("$kind", event is IncomingChatEvent.Application)
            assertEquals(location, (event as IncomingChatEvent.Application).content)
            assertEquals("acct", event.senderKey)
        }
    }

    @Test
    fun `an op on a poll or checklist is never a row`() {
        val vote = receive(body(MessageBodyKind.POLL_VOTE, target = 3L)) as IncomingChatEvent.ContentOp
        assertEquals(ContentOpKind.PollVote, vote.op)
        assertEquals(3L, vote.target)
        assertTrue(vote.choices!!.contentEquals(byteArrayOf(1)))
        val tick = receive(body(MessageBodyKind.CHECKLIST_TICK, target = 4L)) as IncomingChatEvent.ContentOp
        assertEquals(ContentOpKind.ChecklistTick, tick.op)
        assertEquals(ChecklistItemRef.Author(2u), tick.item)
        assertEquals(true, tick.done)
        for ((kind, op) in listOf(
            MessageBodyKind.POLL_CLOSE to ContentOpKind.PollClose,
            MessageBodyKind.CHECKLIST_ADD to ContentOpKind.ChecklistAdd,
            MessageBodyKind.EDIT_CHECKLIST to ContentOpKind.EditChecklist,
            MessageBodyKind.EDIT_ARTICLE to ContentOpKind.EditArticle,
        )) {
            assertEquals(op, (receive(body(kind, target = 1L)) as IncomingChatEvent.ContentOp).op)
        }
    }

    @Test
    fun `the ballot delay is uniform in 0 to 10 seconds inclusive`() {
        val saved = BallotSchedule.random
        try {
            var bound = -1L
            BallotSchedule.random = { b -> bound = b; b - 1 }
            assertEquals(10_000L, BallotSchedule.delayMs())
            assertEquals(10_001L, bound)
            BallotSchedule.random = { 0L }
            assertEquals(0L, BallotSchedule.delayMs())
        } finally {
            BallotSchedule.random = saved
        }
        repeat(1000) {
            val d = BallotSchedule.delayMs()
            assertTrue(d in 0..10_000)
        }
    }

    @Test
    fun `a content row previews as one line per kind`() {
        assertEquals("preview.location" to emptyMap<String, String>(), contentSummary(location))
        val card = MessageContent.ContactCard(uniffi.lakko_ffi_uniffi.SharedContactCard("Ann", "ann", ByteArray(32)))
        assertEquals("preview.contact" to mapOf("name" to "Ann"), contentSummary(card))
    }

    @Test
    fun `an ordinary row is not an anonymous poll`() {
        assertEquals(false, null.isAnonymousPoll())
        assertEquals(false, location.isAnonymousPoll())
    }

    @Test
    fun `bytes compare as unsigned`() {
        assertTrue(compareBytes(byteArrayOf(0x7f), byteArrayOf(0x80.toByte())) < 0)
        assertTrue(compareBytes(byteArrayOf(1), byteArrayOf(1, 0)) < 0)
        assertEquals(0, compareBytes(byteArrayOf(5), byteArrayOf(5)))
    }

    @Test
    fun `an identity that is not a 32-byte key is no voter`() {
        assertNull(identityBytes("not base64!"))
        assertNull(identityBytes(java.util.Base64.getEncoder().encodeToString(ByteArray(31))))
        assertEquals(32, identityBytes(java.util.Base64.getEncoder().encodeToString(ByteArray(32)))!!.size)
    }
}
