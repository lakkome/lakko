// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * ADR-0052 decision 2 / `docs/ux/message-actions.md` sections 5 and 5.1 — the
 * pure rules of a forward: what is carried, what is not, and where a tapped
 * attribution goes.
 */
class ForwardPayloadTest {
    private val origin = "ab".repeat(16)

    private fun message(
        id: Long = 7L,
        text: String = "the words",
        direction: Direction = Direction.Incoming,
        edited: Boolean = false,
        reply: ReplyRef? = null,
        forward: ForwardRef? = null,
        reactions: List<Reaction> = emptyList(),
    ) = ChatMessage(
        messageId = id,
        direction = direction,
        text = text,
        createdAt = "1970-01-01T00:00:00Z",
        authorKey = "Ym9i",
        edited = edited,
        reply = reply,
        forward = forward,
        reactions = reactions,
    )

    // ----- what is sent -----------------------------------------------------

    @Test
    fun `a plain message forwards its text, the author label and this origin`() {
        val (text, ref) = forwardPayloadOf(message(), authorLabel = "Anna", originGroupIdHex = origin)
        assertEquals("the words", text)
        assertEquals(ForwardRef(author = "Anna", originGroupIdHex = origin, originMessageId = 7L), ref)
    }

    @Test
    fun `the edited text is what goes, verbatim`() {
        // An accepted edit replaces the row's text in place, so the row's text
        // IS the edited text (section 5.1: "the edited text if it was edited").
        val (text, _) = forwardPayloadOf(message(text = "corrected", edited = true), "Anna", origin)
        assertEquals("corrected", text)
    }

    @Test
    fun `the reply quote and the reactions are not carried`() {
        val quoted = message(
            reply = ReplyRef(target = 3L, author = "Ivan", excerpt = "earlier"),
            reactions = listOf(Reaction(reactor = "Ym9i", emoji = "👍")),
        )
        val (text, ref) = forwardPayloadOf(quoted, "Anna", origin)
        // Nothing of the quote reaches the forward: the text is the message's
        // own, and the ref has no field for a quote or a reaction at all.
        assertEquals("the words", text)
        assertEquals("Anna", ref.author)
        assertEquals(7L, ref.originMessageId)
    }

    @Test
    fun `a blank author label is no label`() {
        val (_, ref) = forwardPayloadOf(message(), authorLabel = "   ", originGroupIdHex = origin)
        assertNull(ref.author)
        val (_, none) = forwardPayloadOf(message(), authorLabel = null, originGroupIdHex = origin)
        assertNull(none.author)
    }

    @Test
    fun `the label is trimmed as the timeline shows it`() {
        val (_, ref) = forwardPayloadOf(message(), authorLabel = "  You ", originGroupIdHex = origin)
        assertEquals("You", ref.author)
    }

    @Test
    fun `a row with no id names no origin`() {
        // Defence in depth: the menu does not offer Forward on such a row.
        val (_, ref) = forwardPayloadOf(message(id = 0L), "Anna", origin)
        assertEquals("Anna", ref.author)
        assertNull(ref.originGroupIdHex)
        assertNull(ref.originMessageId)
    }

    // ----- forward of a forward --------------------------------------------

    @Test
    fun `a forwarded forward keeps the ORIGINAL author and origin`() {
        val firstOrigin = "cd".repeat(16)
        val source = message(
            id = 40L,
            forward = ForwardRef(author = "Maria", originGroupIdHex = firstOrigin, originMessageId = 12L),
        )
        // The forwarder's own label ("Anna", who sent the forward we are
        // forwarding) and this conversation are NOT what goes: the chain does
        // not rewrite its own history (section 5).
        val (_, ref) = forwardPayloadOf(source, authorLabel = "Anna", originGroupIdHex = origin)
        assertEquals(ForwardRef(author = "Maria", originGroupIdHex = firstOrigin, originMessageId = 12L), ref)
    }

    @Test
    fun `a forward that carried no name is forwarded with no name`() {
        // Falling back to the forwarder's label would be exactly the rewrite
        // the rule forbids, so the absence travels onward.
        val source = message(forward = ForwardRef(author = null, originGroupIdHex = null, originMessageId = null))
        val (_, ref) = forwardPayloadOf(source, authorLabel = "Anna", originGroupIdHex = origin)
        assertNull(ref.author)
        assertNull(ref.originGroupIdHex)
        assertNull(ref.originMessageId)
    }

    // ----- ADR-0053 section 6: nothing out of a one-to-one -------------------

    @Test
    fun `a forward out of a one-to-one carries no origin but keeps the name`() {
        val (text, ref) = forwardPayloadOf(
            message(),
            authorLabel = "Anna",
            originGroupIdHex = origin,
            sourceIsOneToOne = true,
        )
        assertEquals("the words", text)
        assertEquals(ForwardRef(author = "Anna", originGroupIdHex = null, originMessageId = null), ref)
    }

    @Test
    fun `a forwarded forward out of a one-to-one drops the origin it brought along`() {
        val source = message(
            forward = ForwardRef(author = "Maria", originGroupIdHex = "cd".repeat(16), originMessageId = 12L),
        )
        val (_, ref) = forwardPayloadOf(source, authorLabel = "Anna", originGroupIdHex = origin, sourceIsOneToOne = true)
        assertEquals(ForwardRef(author = "Maria", originGroupIdHex = null, originMessageId = null), ref)
    }

    @Test
    fun `a forward out of a group still carries its origin`() {
        val (_, ref) = forwardPayloadOf(message(), authorLabel = "Anna", originGroupIdHex = origin, sourceIsOneToOne = false)
        assertEquals(origin, ref.originGroupIdHex)
        assertEquals(7L, ref.originMessageId)
    }

    // ----- the parsed envelope ---------------------------------------------

    @Test
    fun `an ordinary message parses to no forward`() {
        assertNull(forwardRefFrom(forwardedFrom = null, originGroupB64 = null, originMessageId = null))
        assertNull(forwardRefFrom(forwardedFrom = "  ", originGroupB64 = null, originMessageId = null))
    }

    @Test
    fun `the origin group id crosses as standard base64 and is keyed as hex`() {
        val b64 = forwardOriginGroupIdB64From(origin)
        assertNotNull(b64)
        // Standard alphabet, padded: what `composeForwardEnvelope` expects.
        assertEquals("q6urq6urq6urq6urq6urqw==", b64)
        assertEquals(origin, forwardOriginGroupIdHexFrom(b64!!))
        val ref = forwardRefFrom(forwardedFrom = "Anna", originGroupB64 = b64, originMessageId = 7L)
        assertEquals(ForwardRef(author = "Anna", originGroupIdHex = origin, originMessageId = 7L), ref)
    }

    @Test
    fun `the origin is both or neither`() {
        val b64 = forwardOriginGroupIdB64From(origin)!!
        val idOnly = forwardRefFrom(forwardedFrom = "Anna", originGroupB64 = b64, originMessageId = null)
        assertEquals(ForwardRef(author = "Anna", originGroupIdHex = null, originMessageId = null), idOnly)
        val msgOnly = forwardRefFrom(forwardedFrom = "Anna", originGroupB64 = null, originMessageId = 7L)
        assertEquals(ForwardRef(author = "Anna", originGroupIdHex = null, originMessageId = null), msgOnly)
        // An id that does not decode drops the pair as a whole, never leaving
        // a message id pointing nowhere.
        val broken = forwardRefFrom(forwardedFrom = "Anna", originGroupB64 = "not base64!", originMessageId = 7L)
        assertEquals(ForwardRef(author = "Anna", originGroupIdHex = null, originMessageId = null), broken)
    }

    @Test
    fun `an origin with no name is still a forward`() {
        val b64 = forwardOriginGroupIdB64From(origin)!!
        val ref = forwardRefFrom(forwardedFrom = null, originGroupB64 = b64, originMessageId = 7L)
        assertEquals(ForwardRef(author = null, originGroupIdHex = origin, originMessageId = 7L), ref)
    }

    @Test
    fun `malformed hex is not sent as an origin`() {
        assertNull(forwardOriginGroupIdB64From(""))
        assertNull(forwardOriginGroupIdB64From("abc"))
        assertNull(forwardOriginGroupIdB64From("zz"))
        assertNull(forwardOriginGroupIdHexFrom(""))
    }

    // ----- the line ---------------------------------------------------------

    @Test
    fun `the line names the author or says Forwarded`() {
        assertEquals("Forwarded from Anna", forwardLineLabel(ForwardRef("Anna", null, null)))
        assertEquals("Forwarded", forwardLineLabel(ForwardRef(null, null, null)))
        assertEquals("Forwarded", forwardLineLabel(ForwardRef("  ", null, null)))
    }

    // ----- tap to jump ------------------------------------------------------

    private fun thread(hex: String, vararg ids: Long) = ChatThread(
        groupIdHex = hex,
        messages = ids.map { message(id = it) },
        cursor = 0L,
    )

    @Test
    fun `the tap goes to the original when this device holds the conversation and the message`() {
        val ref = ForwardRef(author = "Anna", originGroupIdHex = origin, originMessageId = 7L)
        assertEquals(origin to 7L, forwardJumpTarget(ref, listOf(thread(origin, 5L, 7L))))
    }

    @Test
    fun `the tap goes nowhere when the conversation is not held`() {
        val ref = ForwardRef(author = "Anna", originGroupIdHex = origin, originMessageId = 7L)
        assertNull(forwardJumpTarget(ref, listOf(thread("ee".repeat(16), 7L))))
        assertNull(forwardJumpTarget(ref, emptyList()))
    }

    @Test
    fun `the tap goes nowhere when the conversation is held but not the message`() {
        val ref = ForwardRef(author = "Anna", originGroupIdHex = origin, originMessageId = 7L)
        assertNull(forwardJumpTarget(ref, listOf(thread(origin, 5L, 6L))))
    }

    @Test
    fun `a forward with no origin has nowhere to go`() {
        val ref = ForwardRef(author = "Anna", originGroupIdHex = null, originMessageId = null)
        assertNull(forwardJumpTarget(ref, listOf(thread(origin, 7L))))
    }
}
