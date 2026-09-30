// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The names an account learned belong to that account, not to the device
 * (2026-09-30: the profiles and verification results lived under one
 * device-wide key, so a second account on the same phone resolved names the
 * first account had learned). Host-side over [InMemoryProfilePersistence],
 * whose [InMemoryProfilePersistence.account] models the signed-in account.
 */
class ProfileStoresAccountScopeTest {

    private val isNewer: (ULong, ULong) -> Boolean = { incoming, stored -> incoming > stored }

    private fun p(name: String, at: ULong, username: String? = null) = PeerProfile(name, username, at)

    @Test
    fun `two accounts never see each other's profiles`() {
        val persistence = InMemoryProfilePersistence(account = "A")
        val store = PeerProfileStore(persistence, isNewer)
        assertEquals(ProfileOffer.STORED, store.offer("KEY", p("Alice as A knows her", 10uL)))

        persistence.account = "B"
        assertNull(store.get("KEY"))
        assertTrue(store.all().isEmpty())
        assertTrue(store.accountIdsByKey().isEmpty())
        // B learns the same person under its own row; an older stamp is new to B.
        assertEquals(ProfileOffer.STORED, store.offer("KEY", p("Alice as B knows her", 5uL)))

        persistence.account = "A"
        assertEquals("Alice as A knows her", store.get("KEY")?.displayName)
        persistence.account = "B"
        assertEquals("Alice as B knows her", store.get("KEY")?.displayName)
    }

    @Test
    fun `with no account a read is empty and an offer is dropped`() {
        val persistence = InMemoryProfilePersistence(account = "A")
        val store = PeerProfileStore(persistence, isNewer)
        store.offer("KEY", p("Alice", 10uL))

        persistence.account = null
        assertNull(store.get("KEY"))
        assertEquals(ProfileOffer.NO_ACCOUNT, store.offer("OTHER", p("Bob", 1uL)))

        persistence.account = "A"
        assertEquals("Alice", store.get("KEY")?.displayName)
        assertNull(store.get("OTHER"))
    }

    @Test
    fun `verification results are per account`() {
        val persistence = InMemoryProfilePersistence(account = "A")
        val store = UsernameVerificationStore(persistence)
        store.put("KEY", "alice", UsernameVerification.VERIFIED)

        persistence.account = "B"
        assertNull(store.get("KEY", "alice"))
        persistence.account = null
        store.put("KEY", "alice", UsernameVerification.FAILED)
        assertNull(store.get("KEY", "alice"))

        persistence.account = "A"
        assertEquals(UsernameVerification.VERIFIED, store.get("KEY", "alice"))
        persistence.account = "B"
        assertNull(store.get("KEY", "alice"))
    }

    @Test
    fun `forgetting one account keeps the other's names`() {
        val persistence = InMemoryProfilePersistence(account = "A")
        val profiles = PeerProfileStore(persistence, isNewer)
        val results = UsernameVerificationStore(persistence)
        profiles.offer("KEY", p("Alice", 10uL))
        results.put("KEY", "alice", UsernameVerification.VERIFIED)
        persistence.account = "B"
        profiles.offer("KEY", p("Alicia", 3uL))
        results.put("KEY", "alice", UsernameVerification.VERIFIED)

        persistence.account = "A"
        profiles.clear("A")
        results.clear("A")
        assertNull(profiles.get("KEY"))
        assertNull(results.get("KEY", "alice"))

        persistence.account = "B"
        assertEquals("Alicia", profiles.get("KEY")?.displayName)
        assertEquals(UsernameVerification.VERIFIED, results.get("KEY", "alice"))
    }
}
