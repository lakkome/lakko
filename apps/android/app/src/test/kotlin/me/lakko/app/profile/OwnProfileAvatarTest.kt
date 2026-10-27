// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0084 — the photo is a profile change like a name, and travels to the
 *  account's other devices with the name's stamp. */
class OwnProfileAvatarTest {
    private val refs = AvatarRefs(avatarBlobOf(ByteArray(32) { 7 }, ByteArray(32), ByteArray(12), 500, 320))
    private val account = ByteArray(16) { 1 }

    @Test
    fun a_new_photo_moves_the_stamp_and_the_name_stamp() {
        val current = OwnProfile("Alice", "alice", updatedAtMs = 100, nameUpdatedAtMs = 100)
        val next = applyOwnProfileChange(current, "Alice", refs, "alice", nowMs = 50)
        assertNotNull(next)
        assertEquals(101L, next!!.updatedAtMs)
        assertEquals(101L, next.nameUpdatedAtMs)
        assertEquals(refs, next.avatar)
    }

    @Test
    fun removing_the_photo_is_a_change_and_nothing_changed_is_null() {
        val current = OwnProfile("Alice", "alice", 100, 100, refs)
        assertNull(applyOwnProfileChange(current, "Alice", refs, "alice", 200))
        val removed = applyOwnProfileChange(current, "Alice", null, "alice", 200)
        assertNull(removed!!.avatar)
        assertEquals(200L, removed.updatedAtMs)
    }

    @Test
    fun a_name_change_keeps_the_photo() {
        val current = OwnProfile("Alice", "alice", 100, 100, refs)
        assertEquals(refs, applyOwnNameChange(current, "Al", "alice", 300)!!.avatar)
        assertEquals(refs, withSessionUsername(current, "alice2", 300)!!.avatar)
    }

    @Test
    fun the_photo_goes_to_groups_and_to_the_other_devices() {
        val profile = OwnProfile("Alice", "alice", 100, 100, refs)
        assertEquals(refs, avatarRefsOf(profile.toFfi(accountId = account).avatar))
        val forDevices = ownProfileForDevices(profile, account)
        assertEquals(refs, avatarRefsOf(forDevices!!.avatar))
        val adopted = adoptOwnProfileFromDevice(OwnProfile("Old", "alice", 50, 50), forDevices, account)
        assertEquals(refs, adopted!!.avatar)
        assertEquals("Alice", adopted.displayName)
    }

    @Test
    fun a_received_profile_keeps_its_photo() {
        val wire = OwnProfile("Bob", "bob", 100, 100, refs).toFfi(accountId = account)
        val peer = peerProfileOf(wire)
        assertEquals(refs, peer.avatar)
        assertTrue(peer.displayName == "Bob")
    }
}
