// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui

import me.lakko.app.ui.chat.canCreateGroup
import me.lakko.app.ui.contacts.contactChatLineKey
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The small screen rules of the 2026-11-01 defect pass. */
class PassDefectRulesTest {

    @Test
    fun `a group starts with a name and nobody else`() {
        assertTrue(canCreateGroup(creating = false, name = "Trip", pickedCount = 0))
        assertTrue(canCreateGroup(creating = false, name = "", pickedCount = 2))
        assertFalse(canCreateGroup(creating = false, name = "  ", pickedCount = 0))
        assertFalse(canCreateGroup(creating = true, name = "Trip", pickedCount = 1))
    }

    @Test
    fun `a contact with a known account is seconds from its chat`() {
        assertEquals("contacts.detail.chat_starting", contactChatLineKey(ByteArray(16)))
        assertEquals("contacts.detail.message_unavailable", contactChatLineKey(null))
    }
}
