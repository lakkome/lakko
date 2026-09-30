// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import java.util.Base64
import me.lakko.app.contacts.AcceptedContactsStore
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * The on-device profile rows are keyed per account and follow the account
 * bound in [AcceptedContactsStore]; the background snapshot reads the account
 * it is handed.
 */
@RunWith(RobolectricTestRunner::class)
class ProfileNamePersistenceScopeTest {

    private val accountA = ByteArray(16) { 0x0A }
    private val accountB = ByteArray(16) { 0x0B }
    private val aB64 = Base64.getEncoder().encodeToString(accountA)
    private val bB64 = Base64.getEncoder().encodeToString(accountB)
    private val peerKey = Base64.getEncoder().encodeToString(ByteArray(32) { 0x33 })

    @Before
    fun setUp() = AcceptedContactsStore.unbind()

    @After
    fun tearDown() = AcceptedContactsStore.unbind()

    @Test
    fun `rows follow the bound account and the snapshot reads the account it is given`() {
        val context = RuntimeEnvironment.getApplication()
        val persistence = SharedPrefsProfilePersistence(context)
        assertNull(persistence.currentAccount())

        AcceptedContactsStore.bind(accountA)
        assertEquals(aB64, persistence.currentAccount())
        val store = PeerProfileStore(persistence) { incoming, stored -> incoming > stored }
        assertEquals(ProfileOffer.STORED, store.offer(peerKey, PeerProfile("Alice", null, 10uL)))

        AcceptedContactsStore.bind(accountB)
        assertNull(store.get(peerKey))
        assertTrue(persistence.loadProfiles(bB64).isEmpty())
        assertEquals("Alice", persistence.loadProfiles(aB64)[peerKey]?.displayName)

        // The background decrypt path names the account explicitly.
        assertTrue(ProfileNameSnapshot.load(context, aB64).isKnown(peerKey))
        assertFalse(ProfileNameSnapshot.load(context, bB64).isKnown(peerKey))
        AcceptedContactsStore.unbind()
        assertFalse(ProfileNameSnapshot.load(context).isKnown(peerKey))
    }

    @Test
    fun `the two accounts' rows live under different keys`() {
        val a = SharedPrefsProfilePersistence.profilesKey(aB64)
        val b = SharedPrefsProfilePersistence.profilesKey(bB64)
        assertFalse(a == b)
        assertTrue(a.endsWith(aB64))
        assertFalse(SharedPrefsProfilePersistence.verificationsKey(aB64) == a)
    }
}
