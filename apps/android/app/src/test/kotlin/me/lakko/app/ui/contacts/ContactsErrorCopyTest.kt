// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import me.lakko.app.contacts.ContactsError
import me.lakko.app.l10n.en
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

/**
 * docs/ux/who-can-find-me.md section 6.1, "Not found": Add by username answers a
 * name that does not resolve the same way whether the account does not exist or
 * does not let the searcher find it (ADR-0060). Rendered from the error's kind
 * (ADR-0061 section 6).
 */
class ContactsErrorCopyTest {
    @Test
    fun `an unknown username reads as not found`() {
        val text = contactsErrorText(ContactsError.usernameUnknown("anna"), "contacts.error.send")
        assertEquals("search.people.not_found", text.key)
        assertEquals("No one found. Check the spelling, or ask them for an invite link.", text.en)
    }

    @Test
    fun `the not found copy never names the username or claims it is unregistered`() {
        val copy = contactsErrorText(ContactsError.usernameUnknown("anna"), "contacts.error.send").en
        assertFalse(copy.contains("anna"))
        assertFalse(copy.contains("registered"))
    }

    @Test
    fun `a refused reach grant reads as request refused`() {
        val error = ContactsError.reachRefused(IllegalStateException("refused"))
        assertEquals("search.people.request_refused", contactsErrorText(error, "contacts.error.send").key)
    }
}
