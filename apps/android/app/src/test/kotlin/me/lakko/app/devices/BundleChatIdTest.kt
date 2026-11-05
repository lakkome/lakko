// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import me.lakko.app.contacts.AcceptedContact
import me.lakko.app.contacts.IncomingContactRequest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * A pairing state bundle carries a contact's chat id only when it is the
 * person's one-to-one on this device (2026-11-05: a recorded group id opened
 * a group from Message).
 */
class BundleChatIdTest {

    @After
    fun reset() {
        StateBundle.contactChat = null
    }

    private fun contact(groupIdHex: String?) = AcceptedContact(
        request = IncomingContactRequest(
            messageId = 1L,
            fromPubkey = ByteArray(32) { 1 },
            fromAccountId = ByteArray(16) { 2 },
            fromPseudonym = ByteArray(32),
            validAfterMs = 1uL,
            intro = null,
            createdAt = "",
        ),
        groupIdHex = groupIdHex,
    )

    @Test
    fun `a recorded id that is not their one-to-one is not carried`() {
        StateBundle.contactChat = { _, _ -> null }
        assertNull(StateBundle.carriedChatId("acct", contact("group1")))
    }

    @Test
    fun `their one-to-one is carried in place of the recorded id`() {
        StateBundle.contactChat = { _, _ -> "one2one" }
        assertEquals("one2one", StateBundle.carriedChatId("acct", contact("group1")))
    }

    @Test
    fun `a failing check carries nothing`() {
        StateBundle.contactChat = { _, _ -> error("roster torn") }
        assertNull(StateBundle.carriedChatId("acct", contact("one2one")))
    }
}
