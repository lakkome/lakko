// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiProfile
import uniffi.lakko_ffi_uniffi.FfiVisibilityMode

/**
 * ADR-0092 section 7 — the "my card may be shared" flag of the own profile:
 * off exactly under `Nobody`, carried on compose, and moved under a strictly
 * newer stamp so receivers (strictly-newer-only) take the change.
 */
class CardShareableTest {
    private val account = ByteArray(16) { 1 }

    private fun own(shareable: Boolean = true, at: Long = 100L) = OwnProfile(
        displayName = "Anna",
        username = "anna",
        updatedAtMs = at,
        nameUpdatedAtMs = 90L,
        cardShareable = shareable,
    )

    @Test
    fun `the flag is off exactly under Nobody`() {
        assertFalse(cardShareableForMode(FfiVisibilityMode.NOBODY))
        assertTrue(cardShareableForMode(FfiVisibilityMode.SOME))
        assertTrue(cardShareableForMode(FfiVisibilityMode.EVERYONE))
        // No setting bound yet reads as shareable.
        assertTrue(cardShareableForMode(null))
    }

    @Test
    fun `compose carries the flag`() {
        assertFalse(own(shareable = false).toFfi(accountId = account).cardShareable)
        assertTrue(own(shareable = true).toFfi(accountId = account).cardShareable)
        assertFalse(ownProfileForDevices(own(shareable = false), account)!!.cardShareable)
    }

    @Test
    fun `a flag change moves the stamp strictly forward and keeps the name stamp`() {
        val changed = withCardShareable(own(at = 100L), shareable = false, nowMs = 50L)
        assertNotNull(changed)
        assertFalse(changed!!.cardShareable)
        assertEquals(101L, changed.updatedAtMs)
        assertEquals(90L, changed.nameUpdatedAtMs)
        assertEquals(500L, withCardShareable(own(at = 100L), false, nowMs = 500L)!!.updatedAtMs)
    }

    @Test
    fun `no change and no profile mean nothing to republish`() {
        assertNull(withCardShareable(own(shareable = false), shareable = false, nowMs = 1_000L))
        assertNull(withCardShareable(null, shareable = false, nowMs = 1_000L))
    }

    @Test
    fun `a name or photo change keeps the flag`() {
        val renamed = applyOwnNameChange(own(shareable = false), "Anna B", "anna", nowMs = 1_000L)
        assertFalse(renamed!!.cardShareable)
    }

    @Test
    fun `a name adopted from another device keeps this device's flag`() {
        val incoming = FfiProfile(
            displayName = "Anna C",
            username = null,
            updatedAtMs = 200uL,
            accountId = account,
            usernameCertificate = null,
            grants = emptyList(),
            cardShareable = true,
        )
        assertFalse(adoptOwnProfileFromDevice(own(shareable = false), incoming, account)!!.cardShareable)
    }

    @Test
    fun `a received profile keeps the flag it carried`() {
        val received = FfiProfile(
            displayName = "Bob",
            username = "bob",
            updatedAtMs = 5uL,
            accountId = null,
            usernameCertificate = null,
            grants = emptyList(),
            cardShareable = false,
        )
        assertFalse(peerProfileOf(received).cardShareable)
        // A row stored before the field, or a found name, is shareable.
        assertTrue(PeerProfile(displayName = "Bob", username = null, updatedAtMs = 0uL).cardShareable)
    }

    @Test
    fun `a newer profile that turns the flag off replaces the held one`() {
        val store = PeerProfileStore(InMemoryProfilePersistence()) { incoming, stored -> incoming > stored }
        store.offer("KEY", PeerProfile("Bob", "bob", 10uL))
        assertTrue(store.get("KEY")!!.cardShareable)
        store.offer("KEY", PeerProfile("Bob", "bob", 11uL, cardShareable = false))
        assertFalse(store.get("KEY")!!.cardShareable)
    }
}
