// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiProfile

/**
 * ADR-0048 section 4a — the own name between the account's own devices: sent
 * with the name's stored stamp, the display name alone; kept by a sibling only
 * when strictly newer and when it names this account.
 */
class OwnProfileDeviceSyncTest {

    private val ownAccount = ByteArray(16) { (it + 1).toByte() }
    private val otherAccount = ByteArray(16) { (0x40 + it).toByte() }

    private fun incoming(name: String?, stamp: ULong, accountId: ByteArray? = ownAccount) = FfiProfile(
        displayName = name,
        username = "someone_else",
        updatedAtMs = stamp,
        accountId = accountId,
        usernameCertificate = null,
        grants = emptyList(),
    )

    // ---------------------------------------------------------------- receive

    @Test
    fun `a strictly newer name is adopted with its stamp as is`() {
        val stored = OwnProfile("Old", "alice", updatedAtMs = 100L, nameUpdatedAtMs = 100L)
        val adopted = adoptOwnProfileFromDevice(stored, incoming("Alice", 150uL), ownAccount)
        assertEquals("Alice", adopted?.displayName)
        assertEquals(150L, adopted?.nameUpdatedAtMs)
        // The username stays the session's, never the envelope's.
        assertEquals("alice", adopted?.username)
    }

    @Test
    fun `an equal or older name is ignored`() {
        val stored = OwnProfile("Alice", "alice", updatedAtMs = 100L, nameUpdatedAtMs = 100L)
        assertNull(adoptOwnProfileFromDevice(stored, incoming("Other", 100uL), ownAccount))
        assertNull(adoptOwnProfileFromDevice(stored, incoming("Other", 99uL), ownAccount))
    }

    @Test
    fun `a profile naming another account is dropped`() {
        assertNull(adoptOwnProfileFromDevice(null, incoming("Mallory", 500uL, otherAccount), ownAccount))
        // One that states no account id is this account's by the carrier.
        assertEquals(
            "Alice",
            adoptOwnProfileFromDevice(null, incoming("Alice", 500uL, accountId = null), ownAccount)?.displayName,
        )
    }

    @Test
    fun `a cleared name is adopted when newer`() {
        val stored = OwnProfile("Alice", "alice", updatedAtMs = 100L, nameUpdatedAtMs = 100L)
        val adopted = adoptOwnProfileFromDevice(stored, incoming(null, 200uL), ownAccount)
        assertTrue(adopted != null)
        assertNull(adopted?.displayName)
        assertEquals(200L, adopted?.nameUpdatedAtMs)
    }

    @Test
    fun `a fresh device's sign-in stamp does not outrank a name saved earlier elsewhere`() {
        // A device that just signed in stated its username at t=900 but never
        // saved a name: a name saved at t=300 on the first device is taken.
        val fresh = OwnProfile(null, "alice", updatedAtMs = 900L)
        assertEquals(0L, fresh.nameUpdatedAtMs)
        val adopted = adoptOwnProfileFromDevice(fresh, incoming("Alice", 300uL), ownAccount)
        assertEquals("Alice", adopted?.displayName)
        assertEquals(300L, adopted?.nameUpdatedAtMs)
        // The profile stamp never goes back.
        assertEquals(900L, adopted?.updatedAtMs)
    }

    // ------------------------------------------------------------------- send

    @Test
    fun `the compose side uses the stored name stamp and the name alone`() {
        val profile = OwnProfile("Alice", "alice", updatedAtMs = 900L, nameUpdatedAtMs = 42L)
        var composed: FfiProfile? = null
        val wire = ownProfileDeviceEnvelope(profile, ownAccount) {
            composed = it
            byteArrayOf(7)
        }
        assertArrayEquals(byteArrayOf(7), wire)
        val sent = composed!!
        assertEquals(42uL, sent.updatedAtMs)
        assertEquals("Alice", sent.displayName)
        assertNull(sent.username)
        assertNull(sent.usernameCertificate)
        assertTrue(sent.grants.isEmpty())
        assertArrayEquals(ownAccount, sent.accountId)
    }

    @Test
    fun `a cleared name is sent, one never saved is not`() {
        val cleared = OwnProfile(null, "alice", updatedAtMs = 60L, nameUpdatedAtMs = 60L)
        assertEquals(60uL, ownProfileForDevices(cleared, ownAccount)?.updatedAtMs)
        assertNull(ownProfileForDevices(cleared, ownAccount)?.displayName)
        assertNull(ownProfileForDevices(OwnProfile(null, "alice", updatedAtMs = 60L), ownAccount))
        assertNull(ownProfileForDevices(null, ownAccount))
    }

    @Test
    fun `a composer refusal sends nothing`() {
        val profile = OwnProfile("Alice", null, updatedAtMs = 5L)
        assertNull(ownProfileDeviceEnvelope(profile, ownAccount) { throw IllegalStateException("refused") })
    }

    // ---------------------------------------------------- the name's own stamp

    @Test
    fun `a name change moves the name stamp, a username change does not`() {
        val current = OwnProfile("Alice", "alice", updatedAtMs = 10L, nameUpdatedAtMs = 10L)
        val renamed = applyOwnNameChange(current, "Alice P", "alice", nowMs = 20L)
        assertEquals(20L, renamed?.nameUpdatedAtMs)
        val cleared = applyOwnNameChange(current, null, "alice", nowMs = 30L)
        assertEquals(30L, cleared?.nameUpdatedAtMs)
        val usernameOnly = applyOwnNameChange(current, "Alice", "alice2", nowMs = 40L)
        assertEquals(40L, usernameOnly?.updatedAtMs)
        assertEquals(10L, usernameOnly?.nameUpdatedAtMs)
    }

    @Test
    fun `a save after an adopted name is stamped strictly after it`() {
        // Adopted at 500 while this device's own clock reads 100.
        val adopted = OwnProfile("Alice", "alice", updatedAtMs = 500L, nameUpdatedAtMs = 500L)
        val next = applyOwnNameChange(adopted, "Bob", "alice", nowMs = 100L)
        assertEquals(501L, next?.nameUpdatedAtMs)
    }

    @Test
    fun anUneditedFieldFollowsANameFromAnotherDeviceAndAnEditedOneDoesNot() {
        assertEquals("Alice", followSavedName(draft = "", mirrored = "", incoming = "Alice"))
        assertEquals("Alicia", followSavedName(draft = "Alicia", mirrored = "Alice", incoming = "Alice B"))
    }

    @Test
    fun aDeviceWithoutAProfileStatesItsUsernameAtStampZero() {
        val fresh = withSessionUsername(current = null, username = "alice", nowMs = 900L)
        assertEquals(OwnProfile(displayName = null, username = "alice", updatedAtMs = 0L, nameUpdatedAtMs = 0L), fresh)
        // A name another device saved at 300 still outranks it at every member.
        assertTrue(300L > fresh!!.updatedAtMs)
        assertNull(withSessionUsername(current = fresh, username = "alice", nowMs = 950L))

        val named = OwnProfile(displayName = "Alice", username = null, updatedAtMs = 500L, nameUpdatedAtMs = 500L)
        val claimed = withSessionUsername(current = named, username = "alice", nowMs = 900L)!!
        assertEquals(900L, claimed.updatedAtMs)
        assertEquals(500L, claimed.nameUpdatedAtMs)
        assertEquals("Alice", claimed.displayName)
    }
}
