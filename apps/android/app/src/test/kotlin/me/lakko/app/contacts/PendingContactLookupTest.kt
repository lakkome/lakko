// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * docs/ux/navigation.md section 4.3 — a found username on its way to Contacts.
 *
 * A one-shot slot: if it were not consumed, the add sheet would re-open on
 * every later visit to the tab, which reads as the app deciding for you.
 */
class PendingContactLookupTest {

    @After
    fun tearDown() {
        PendingContactLookup.consume()
    }

    @Test
    fun `a published name waits to be taken`() {
        PendingContactLookup.publish("anna")
        assertEquals("anna", PendingContactLookup.pendingUsername.value)
    }

    @Test
    fun `consuming empties the slot`() {
        PendingContactLookup.publish("anna")
        PendingContactLookup.consume()
        assertNull(PendingContactLookup.pendingUsername.value)
    }

    @Test
    fun `surrounding space is not part of a name`() {
        PendingContactLookup.publish("  anna  ")
        assertEquals("anna", PendingContactLookup.pendingUsername.value)
    }

    @Test
    fun `a blank name is not a name and never opens the sheet`() {
        PendingContactLookup.publish("   ")
        assertNull(PendingContactLookup.pendingUsername.value)
    }

    @Test
    fun `a second name replaces the first rather than queueing behind it`() {
        PendingContactLookup.publish("anna")
        PendingContactLookup.publish("boris")
        assertEquals("boris", PendingContactLookup.pendingUsername.value)
    }
}
