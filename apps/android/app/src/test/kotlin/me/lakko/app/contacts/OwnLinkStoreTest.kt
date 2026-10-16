// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0078 section 10 - the records of the links this device minted: the
 * only list of the account's links and the only revocation keys, per
 * account and per ledger.
 */
class OwnLinkStoreTest {

    private val blobId = ByteArray(6) { it.toByte() }
    private val seed = ByteArray(32) { 7 }

    @Test
    fun `a record keeps the ledger id and the revocation seed`() {
        val link = OwnLinks.record(
            kind = OwnLink.KIND_CONTACT, blobId = blobId, seed = seed, label = "team",
            expiryMs = 5_000, maxUses = 3, nowMs = 1_000,
        )
        assertArrayEquals(blobId, link.blobId())
        assertArrayEquals(seed, link.seed())
        assertEquals(1_000, link.mintedAtMs)
        assertFalse(link.isExpired(4_999))
        assertTrue(link.isExpired(5_000))
        assertFalse("0 never expires", link.copy(expiryMs = 0).isExpired(Long.MAX_VALUE))
        assertNull("a seed of another length is refused", link.copy(revocationSeed = "AAAA").seed())
    }

    @Test
    fun `records are kept per account and per ledger`() {
        val store = InMemoryOwnLinkStore()
        val contact = OwnLinks.record(OwnLink.KIND_CONTACT, blobId, seed)
        val group = OwnLinks.record(OwnLink.KIND_GROUP, blobId, ByteArray(32) { 9 })
        store.put("alice", contact)
        store.put("alice", group)
        store.put("bob", contact)
        assertEquals(2, store.all("alice").size)
        assertArrayEquals(ByteArray(32) { 9 }, store.get("alice", OwnLink.KIND_GROUP, contact.slug)!!.seed())
        store.remove("alice", OwnLink.KIND_CONTACT, contact.slug)
        assertNull(store.get("alice", OwnLink.KIND_CONTACT, contact.slug))
        assertTrue("the same id in another ledger stays", store.get("alice", OwnLink.KIND_GROUP, contact.slug) != null)
        store.clearForAccount("alice")
        assertTrue(store.all("alice").isEmpty())
        assertEquals("another account's records stay", 1, store.all("bob").size)
    }
}
