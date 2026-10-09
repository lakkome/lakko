// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.auth.MarkerStorage
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * ADR-0066 part 3 - the account's mailbox follows its published key, and a
 * mailbox's message ids are its own: the cursor kept from before a rotation
 * must not be used to read the new key's mailbox.
 */
class InboxMailboxTest {
    private class MapStorage : MarkerStorage {
        val map = mutableMapOf<String, String>()
        override fun get(key: String): String? = map[key]
        override fun set(key: String, value: String) {
            map[key] = value
        }
        override fun remove(key: String) {
            map.remove(key)
        }
    }

    @Test
    fun `a device that never saw another key reads from its kept cursor`() {
        assertEquals(InboxRead(41, usesKeptCursor = true, keptCursorOwnerB64 = null), inboxReadFor(null, "K1", 41, 0))
        assertEquals(InboxRead(41, usesKeptCursor = true, keptCursorOwnerB64 = null), inboxReadFor("K1", "K1", 41, 7))
    }

    @Test
    fun `after a rotation the new mailbox starts at its own cursor, not the kept one`() {
        // The kept cursor (41) belongs to K1's mailbox; K2's starts at 0.
        assertEquals(InboxRead(0, usesKeptCursor = false, keptCursorOwnerB64 = "K1"), inboxReadFor("K1", "K2", 41, 0))
        assertEquals(InboxRead(5, usesKeptCursor = false, keptCursorOwnerB64 = "K1"), inboxReadFor("K1", "K2", 41, 5))
    }

    @Test
    fun `the store keeps the owner key and a forward-only cursor per later mailbox`() {
        val store = InboxMailboxStore(MapStorage())
        assertNull(store.ownerKey("acct"))
        store.setOwnerKey("acct", "K1")
        assertEquals("K1", store.ownerKey("acct"))
        assertEquals(0L, store.cursor("acct", "K2"))
        store.advance("acct", "K2", 9)
        store.advance("acct", "K2", 4)
        assertEquals(9L, store.cursor("acct", "K2"))
        // Another account and another mailbox are their own.
        assertEquals(0L, store.cursor("other", "K2"))
        assertEquals(0L, store.cursor("acct", "K3"))
    }
}
