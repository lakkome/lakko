// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0024 — durable read watermarks. Run as `:app:testDebugUnitTest`.
 *
 *  1. The merge is monotone: a lower value never lowers a stored one, and the
 *     result does not depend on write order.
 *  2. Accounts (and groups) are isolated from each other, and deleting one
 *     account leaves the others intact.
 *  3. The codec the `SharedPreferences` store writes round-trips, and a
 *     malformed row reads as empty.
 */
class ReadWatermarkStoreTest {

    private val alice = "YWxpY2Utc2lnbmF0dXJlLWtleS0zMi1ieXRlcy0hISE="
    private val bob = "Ym9iLXNpZ25hdHVyZS1rZXktMzItYnl0ZXMtLS0tLS0="
    private val accountA = "YWNjb3VudC1BLTE2LWJ5dGVz"
    private val accountB = "YWNjb3VudC1CLTE2LWJ5dGVz"
    private val group1 = "0a0b0c0d"
    private val group2 = "1a1b1c1d"

    @Test
    fun `merge never lowers a stored watermark`() {
        val store = InMemoryReadWatermarkStore()
        store.merge(accountA, group1, mapOf(alice to 10L, bob to 4L))
        val after = store.merge(accountA, group1, mapOf(alice to 7L, bob to 9L))
        assertEquals(mapOf(alice to 10L, bob to 9L), after)
        assertEquals(mapOf(alice to 10L, bob to 9L), store.load(accountA, group1))
    }

    @Test
    fun `merge converges whatever order writes land in`() {
        val writes = listOf(mapOf(alice to 3L), mapOf(alice to 8L, bob to 2L), mapOf(bob to 5L))
        val forward = InMemoryReadWatermarkStore()
        val backward = InMemoryReadWatermarkStore()
        writes.forEach { forward.merge(accountA, group1, it) }
        writes.reversed().forEach { backward.merge(accountA, group1, it) }
        assertEquals(forward.load(accountA, group1), backward.load(accountA, group1))
        assertEquals(mapOf(alice to 8L, bob to 5L), forward.load(accountA, group1))
    }

    @Test
    fun `pure merge rule matches the fold`() {
        val stored = mapOf(alice to 5L)
        assertEquals(stored, mergeReadWatermarks(stored, mapOf(alice to 5L)))
        assertEquals(mapOf(alice to 6L, bob to 1L), mergeReadWatermarks(stored, mapOf(alice to 6L, bob to 1L)))
    }

    @Test
    fun `accounts and groups are isolated`() {
        val store = InMemoryReadWatermarkStore()
        store.merge(accountA, group1, mapOf(alice to 10L))
        store.merge(accountA, group2, mapOf(bob to 3L))
        store.merge(accountB, group1, mapOf(alice to 1L))

        assertEquals(mapOf(alice to 10L), store.load(accountA, group1))
        assertEquals(mapOf(alice to 1L), store.load(accountB, group1))
        assertEquals(
            mapOf(group1 to mapOf(alice to 10L), group2 to mapOf(bob to 3L)),
            store.loadAccount(accountA),
        )

        store.deleteAccount(accountA)
        assertTrue(store.load(accountA, group1).isEmpty())
        assertTrue(store.loadAccount(accountA).isEmpty())
        assertEquals(mapOf(alice to 1L), store.load(accountB, group1))
    }

    @Test
    fun `group id case does not split a row`() {
        val store = InMemoryReadWatermarkStore()
        store.merge(accountA, "0A0B0C0D", mapOf(alice to 2L))
        assertEquals(mapOf(alice to 2L), store.load(accountA, group1))
    }

    @Test
    fun `codec round-trips and reads garbage as empty`() {
        val value = mapOf(alice to 42L, bob to Long.MAX_VALUE)
        assertEquals(value, decodeReadWatermarks(encodeReadWatermarks(value)))
        assertEquals(emptyMap<String, Long>(), decodeReadWatermarks(encodeReadWatermarks(emptyMap())))
        assertTrue(decodeReadWatermarks(null).isEmpty())
        assertTrue(decodeReadWatermarks("").isEmpty())
        assertTrue(decodeReadWatermarks("not json").isEmpty())
        assertTrue(decodeReadWatermarks("""{"$alice":"ten"}""").isEmpty())
    }

    @Test
    fun `a relaunch reads back what was persisted`() {
        // The production store persists exactly the encoded row; simulate a
        // process restart by decoding it into a fresh store's seed.
        val before = InMemoryReadWatermarkStore()
        before.merge(accountA, group1, mapOf(alice to 12L, bob to 11L))
        val row = encodeReadWatermarks(before.load(accountA, group1))

        val after = InMemoryReadWatermarkStore()
        after.merge(accountA, group1, decodeReadWatermarks(row))
        assertEquals(mapOf(alice to 12L, bob to 11L), after.load(accountA, group1))
    }
}
