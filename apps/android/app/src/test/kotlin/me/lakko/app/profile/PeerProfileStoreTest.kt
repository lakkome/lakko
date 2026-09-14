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
}
