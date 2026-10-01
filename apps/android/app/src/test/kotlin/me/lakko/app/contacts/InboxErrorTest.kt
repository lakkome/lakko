// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * ADR-0061 section 6 — the contacts screen renders the inbox error by its
 * kind. A Welcome that could not be joined used to share the poll failure's
 * slot and read "Couldn't check for new contact requests".
 */
class InboxErrorTest {
    @Test
    fun `each inbox error renders its own key`() {
        assertEquals("contacts.mailbox.poll_failed", InboxError.POLL_FAILED.key)
        assertEquals("contacts.mailbox.chat_join_failed", InboxError.WELCOME_JOIN_FAILED.key)
    }
}
