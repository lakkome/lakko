// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import me.lakko.app.chat.ConversationKind
import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test

/** A message notification: the conversation's name as the title; in a group
 *  `Sender: text`, in a one-to-one the text. A sender with no stored name
 *  gives no shape (the generic banner instead), never a placeholder word. */
class NotificationShapeTest {

    @Before
    fun english() {
        EnglishForTests.install()
    }

    @Test
    fun `a group names itself and prefixes the sender`() {
        assertEquals("Hiking" to "Anna: hello", notificationShape(ConversationKind.Group, "Hiking", "Anna", "hello"))
        assertEquals("Lakko" to "Anna: hello", notificationShape(ConversationKind.Group, null, "Anna", "hello"))
        assertEquals("Hiking" to "Anna: 2 attachments", notificationShape(ConversationKind.Group, " Hiking ", "Anna", "2 attachments"))
    }

    @Test
    fun `a one-to-one is titled by the person and shows the text alone`() {
        assertEquals("Anna" to "hello", notificationShape(ConversationKind.Chat, null, "Anna", "hello"))
    }

    @Test
    fun `a sender with no name gives no shape, in every kind of conversation`() {
        for (kind in listOf(ConversationKind.Group, ConversationKind.Chat, null)) {
            assertNull(notificationShape(kind, "Hiking", null, "hello"))
            assertNull(notificationShape(kind, "Hiking", "   ", "hello"))
        }
    }

    @Test
    fun `the generic banner of an unnamed sender carries no placeholder word`() {
        val generic = unnamedSenderPreview("ab")
        assertEquals("Lakko", generic.title)
        assertEquals(EnglishForTests.strings["notification.message.new"], generic.body)
        assertNoPlaceholderWord(generic.title, generic.body)
    }

    @Test
    fun `a line that names the sender gets no prefix, and an unread group still names the sender`() {
        assertEquals(
            "Hiking" to "Anna reacted to your message",
            notificationShape(ConversationKind.Group, "Hiking", "Anna", "Anna reacted to your message", prefixSender = false),
        )
        assertEquals("Lakko" to "Anna: hello", notificationShape(null, "Hiking", "Anna", "hello"))
    }
}
