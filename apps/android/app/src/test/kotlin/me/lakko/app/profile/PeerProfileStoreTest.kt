// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * ADR-0048 §3 — the per-signature-key profile store: strictly newer only, and
 * keyed on the key the caller resolved at receipt.
 *
 * `isNewer` is a stand-in for the core's `profileIsNewer` with the same
 * contract (strictly greater; equal is not newer).
 */
class PeerProfileStoreTest {

    private val isNewer: (ULong, ULong) -> Boolean = { incoming, stored -> incoming > stored }

    private fun p(name: String, at: ULong, username: String? = null) = PeerProfile(name, username, at)

    @Test
    fun `a first profile is stored and a strictly newer one replaces it`() {
        val store = PeerProfileStore(InMemoryProfilePersistence(), isNewer)
        assertEquals(ProfileOffer.STORED, store.offer("KEY_A", p("Alice", 10uL)))
        assertEquals(ProfileOffer.STORED, store.offer("KEY_A", p("Alice P", 11uL)))
        assertEquals("Alice P", store.get("KEY_A")?.displayName)
    }

    @Test
    fun `an equal or older stamp is not an update`() {
        val store = PeerProfileStore(InMemoryProfilePersistence(), isNewer)
        store.offer("KEY_A", p("Alice", 10uL))
        assertEquals(ProfileOffer.NOT_NEWER, store.offer("KEY_A", p("Replay", 10uL)))
        assertEquals(ProfileOffer.NOT_NEWER, store.offer("KEY_A", p("Stale", 9uL)))
        assertEquals("Alice", store.get("KEY_A")?.displayName)
    }

    @Test
    fun `a same-stamp resend fills in a missing account id and nothing else`() {
        val store = PeerProfileStore(InMemoryProfilePersistence(), isNewer)
        store.offer("KEY_A", p("Alice", 10uL))
        val id = "0".repeat(31) + "5"
        assertEquals(
            ProfileOffer.STORED,
            store.offer("KEY_A", p("Mallory", 10uL).copy(accountIdHex = id)),
        )
        assertEquals("Alice", store.get("KEY_A")?.displayName)
        assertEquals(id, store.get("KEY_A")?.accountIdHex)
        // An older stamp learns nothing; a known id is never replaced.
        val other = "0".repeat(31) + "6"
        assertEquals(ProfileOffer.NOT_NEWER, store.offer("KEY_A", p("Alice", 9uL).copy(accountIdHex = other)))
        assertEquals(ProfileOffer.NOT_NEWER, store.offer("KEY_A", p("Alice", 10uL).copy(accountIdHex = other)))
        assertEquals(id, store.get("KEY_A")?.accountIdHex)
    }

    @Test
    fun `profiles are keyed per signature key and never cross`() {
        val store = PeerProfileStore(InMemoryProfilePersistence(), isNewer)
        store.offer("KEY_A", p("Alice", 100uL))
        // A LOWER stamp from a DIFFERENT key is stored: the order is per key.
        assertEquals(ProfileOffer.STORED, store.offer("KEY_B", p("Bob", 1uL)))
        assertEquals("Alice", store.get("KEY_A")?.displayName)
        assertEquals("Bob", store.get("KEY_B")?.displayName)
    }

    @Test
    fun `an unattributed profile is never stored`() {
        val store = PeerProfileStore(InMemoryProfilePersistence(), isNewer)
        assertEquals(ProfileOffer.UNATTRIBUTED, store.offer(null, p("Mallory", 1uL)))
        assertEquals(ProfileOffer.UNATTRIBUTED, store.offer("", p("Mallory", 1uL)))
        assertEquals(emptyMap<String, PeerProfile>(), store.all())
    }

    @Test
    fun `the store survives a reload from its persistence`() {
        val persistence = InMemoryProfilePersistence()
        PeerProfileStore(persistence, isNewer).offer("KEY_A", p("Alice", 5uL, "alice"))
        val reloaded = PeerProfileStore(persistence, isNewer)
        assertEquals(p("Alice", 5uL, "alice"), reloaded.get("KEY_A"))
        assertEquals(ProfileOffer.NOT_NEWER, reloaded.offer("KEY_A", p("Alice", 5uL)))
    }

    @Test
    fun `the directory stores under the given key and reports what it did`() {
        val directory = ProfileDirectory(
            persistence = InMemoryProfilePersistence(),
            scope = CoroutineScope(Dispatchers.Unconfined),
            isNewer = isNewer,
            passesRule = { true },
        )
        val before = directory.version.value
        assertEquals(ProfileOffer.STORED, directory.onProfileReceived("KEY_A", p("Alice", 2uL)))
        assertEquals(before + 1, directory.version.value)
        assertEquals(ProfileOffer.NOT_NEWER, directory.onProfileReceived("KEY_A", p("Old", 1uL)))
        assertEquals(before + 1, directory.version.value)
        assertEquals(ProfileOffer.UNATTRIBUTED, directory.onProfileReceived(null, p("X", 9uL)))
        assertNull(directory.profiles.get("KEY_B"))
    }

    // ADR-0048 v2 — the account id a profile carries, kept per key for the
    // ADR-0021 call roster.

    private val idA = "0a".repeat(16)

    @Test
    fun `a newer profile without an account id keeps the id already held`() {
        val store = PeerProfileStore(InMemoryProfilePersistence(), isNewer)
        store.offer("KEY_A", PeerProfile("Alice", null, 1uL, accountIdHex = idA))
        assertEquals(ProfileOffer.STORED, store.offer("KEY_A", PeerProfile("Alice P", null, 2uL)))
        assertEquals("Alice P", store.get("KEY_A")?.displayName)
        assertEquals(idA, store.get("KEY_A")?.accountIdHex)
    }

    @Test
    fun `a newer profile with an account id replaces the held one`() {
        val store = PeerProfileStore(InMemoryProfilePersistence(), isNewer)
        val idB = "0b".repeat(16)
        store.offer("KEY_A", PeerProfile("Alice", null, 1uL, accountIdHex = idA))
        store.offer("KEY_A", PeerProfile("Alice", null, 2uL, accountIdHex = idB))
        assertEquals(idB, store.get("KEY_A")?.accountIdHex)
    }

    @Test
    fun `the directory enumerates only the profiles that carry an account id`() {
        val directory = ProfileDirectory(
            persistence = InMemoryProfilePersistence(),
            scope = CoroutineScope(Dispatchers.Unconfined),
            isNewer = isNewer,
            passesRule = { true },
        )
        directory.onProfileReceived("KEY_A", PeerProfile("Alice", null, 1uL, accountIdHex = idA))
        directory.onProfileReceived("KEY_B", PeerProfile("Bob", null, 1uL))
        assertEquals(mapOf("KEY_A" to idA), directory.accountIdsByKey())
    }

    @Test
    fun `the raw id becomes lowercase hex only when it is exactly sixteen bytes`() {
        assertEquals("ff".repeat(16), accountIdHexOrNull(ByteArray(16) { 0xff.toByte() }))
        assertNull(accountIdHexOrNull(ByteArray(15)))
        assertNull(accountIdHexOrNull(ByteArray(17)))
        assertNull(accountIdHexOrNull(null))
    }
}
