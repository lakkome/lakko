// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.ui.contacts.MessageAction
import me.lakko.app.ui.contacts.messageAction
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * New chat (maintainer, 2026-11-05): a person in contacts can always be
 * messaged. With no chat, picking them starts a new one-to-one; only a contact
 * with no account id stays unavailable.
 */
class NewChatPickRowTest {

    @Test
    fun `a contact with a chat is picked to open it`() {
        val row = newChatPickRow(messageAction("aa", ByteArray(16)), starting = false, line = null)
        assertTrue(row.enabled)
        assertFalse(row.dimmed)
        assertNull(row.lineKey)
    }

    @Test
    fun `a contact with an account id and no chat is not shown as unavailable`() {
        val action = messageAction(null, ByteArray(16))
        assertEquals(MessageAction.Found, action)
        val row = newChatPickRow(action, starting = false, line = null)
        assertTrue(row.enabled)
        assertFalse(row.dimmed)
        assertFalse(row.busy)
        assertNull(row.lineKey)
    }

    @Test
    fun `while the one-to-one is founded the row shows progress and takes no second pick`() {
        val row = newChatPickRow(MessageAction.Found, starting = true, line = null)
        assertTrue(row.busy)
        assertFalse(row.enabled)
        assertNull(row.lineKey)
    }

    @Test
    fun `a failed founding says so and the row is usable again`() {
        val row = newChatPickRow(
            MessageAction.Found,
            starting = false,
            line = me.lakko.app.ui.contacts.foundLine(false, me.lakko.app.ui.contacts.MessageNotice.START_FAILED, false),
        )
        assertTrue(row.enabled)
        assertFalse(row.busy)
        assertEquals("contacts.error.start_chat", row.lineKey)
        assertTrue(row.lineIsError)
    }

    @Test
    fun `only a contact with no account id stays unavailable`() {
        val row = newChatPickRow(messageAction(null, null), starting = false, line = null)
        assertFalse(row.enabled)
        assertTrue(row.dimmed)
        assertEquals("chats.new_chat.not_available", row.lineKey)
    }
}
