// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import me.lakko.app.contacts.ContactsError
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

/**
 * docs/ux/who-can-find-me.md section 6.1, "Not found": Add by username answers a
 * name that does not resolve the same way whether the account does not exist or
 * does not let the searcher find it (ADR-0060).
 */
class ContactsErrorCopyTest {
    @Test
    fun `an unknown username reads as not found`() {
        assertEquals(
            "No one found. Check the spelling, or ask them for an invite link.",
            contactsErrorCopy(ContactsError.usernameUnknown("anna")),
        )
    }

    @Test
    fun `the not found copy never names the username or claims it is unregistered`() {
        val copy = contactsErrorCopy(ContactsError.usernameUnknown("anna"))
        assertFalse(copy.contains("anna"))
        assertFalse(copy.contains("registered"))
    }
}
