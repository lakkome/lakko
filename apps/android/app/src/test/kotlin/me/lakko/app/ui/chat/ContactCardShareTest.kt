// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import java.util.Base64
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.Direction
import me.lakko.app.chat.isForwardableContent
import me.lakko.app.chat.isForwardableMessage
import me.lakko.app.contacts.AcceptedContact
import me.lakko.app.contacts.IncomingContactRequest
import me.lakko.app.profile.PeerProfile
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.MessageContent

/**
 * `docs/ux/attachments.md` sections 10.1 and 11a — the contact picker reads
 * the newest profile held from each contact, and Forward is offered by kind.
 */
class ContactCardShareTest {
    private val bobKey = ByteArray(32) { 7 }
    private val bobB64 = Base64.getEncoder().encodeToString(bobKey)

    private fun contact(requestProfile: PeerProfile?) = AcceptedContact(
        request = IncomingContactRequest(
            messageId = 1L,
            fromPubkey = bobKey,
            fromAccountId = null,
            fromPseudonym = ByteArray(32),
            validAfterMs = 0uL,
            intro = null,
            createdAt = "2026-11-18T10:00:00Z",
            profile = requestProfile,
        ),
    )

    @Test
    fun `the username comes from the newest stored profile, not the request`() {
        val rows = contactPickRows(listOf(contact(PeerProfile("Bob", "bob_old", 1uL)))) { key ->
            if (key == bobB64) PeerProfile("Bob", "bob_new", 9uL) else null
        }
        assertEquals("bob_new", rows.single().username)
        assertTrue(rows.single().pickable)
    }

    @Test
    fun `a newest profile with no username leaves the row without one`() {
        val rows = contactPickRows(listOf(contact(PeerProfile("Bob", "bob_old", 1uL)))) {
            PeerProfile("Bob", null, 9uL)
        }
        assertNull(rows.single().username)
        assertFalse(rows.single().pickable)
    }

    @Test
    fun `with nothing stored the request profile is the newest there is`() {
        val rows = contactPickRows(listOf(contact(PeerProfile("Bob", "bob", 1uL)))) { null }
        assertEquals("bob", rows.single().username)
        assertTrue(rows.single().shareable)
        assertTrue(rows.single().pickable)
    }

    @Test
    fun `a contact on Nobody is shown but cannot be picked`() {
        val rows = contactPickRows(listOf(contact(null))) {
            PeerProfile("Bob", "bob", 9uL, cardShareable = false)
        }
        val row = rows.single()
        assertEquals("bob", row.username)
        assertFalse(row.shareable)
        assertFalse(row.pickable)
    }

    private fun message(content: MessageContent?) = ChatMessage(
        messageId = 3L,
        direction = Direction.Incoming,
        text = "",
        attachments = emptyList(),
        createdAt = "1970-01-01T00:00:00Z",
        authorKey = "Ym9i",
        content = content,
    )

    @Test
    fun `forward is offered for location, article and shareable cards only`() {
        val yes: (ByteArray) -> Boolean = { true }
        val location = MessageContent.Location(
            uniffi.lakko_ffi_uniffi.SharedLocation(latE7 = 1, lonE7 = 2, accuracyM = 3u, fixAtMs = 4L, name = null),
        )
        val article = MessageContent.Article(uniffi.lakko_ffi_uniffi.Article(title = "T", blocks = emptyList()))
        val card = MessageContent.ContactCard(uniffi.lakko_ffi_uniffi.SharedContactCard("Bob", "bob", bobKey))
        val checklist = MessageContent.Checklist(
            uniffi.lakko_ffi_uniffi.SignedChecklist(
                uniffi.lakko_ffi_uniffi.ChecklistBody("x", false, false, emptyList()),
                null,
            ),
        )
        val poll = MessageContent.Poll(
            uniffi.lakko_ffi_uniffi.SignedPoll(
                uniffi.lakko_ffi_uniffi.PollBody(
                    id = ByteArray(16),
                    question = "q",
                    options = listOf("a", "b"),
                    multiple = false,
                    quiz = null,
                    closeAtMs = null,
                    anonymous = null,
                ),
                null,
            ),
        )
        assertTrue(isForwardableMessage(message(null), yes))
        assertTrue(isForwardableContent(location, yes))
        assertTrue(isForwardableContent(article, yes))
        assertTrue(isForwardableContent(card, yes))
        assertFalse(isForwardableContent(poll, yes))
        assertFalse(isForwardableContent(checklist, yes))
        // The card of a person whose newest profile says no.
        var asked: ByteArray? = null
        assertFalse(isForwardableContent(card) { key -> asked = key; false })
        assertTrue(asked!!.contentEquals(bobKey))
    }
}
