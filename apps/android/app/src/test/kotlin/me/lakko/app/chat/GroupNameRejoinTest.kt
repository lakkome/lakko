// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.push.GroupPushRouting
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * ADR-0051 receive gate across a removal and a re-add (ADR-0087 section 9):
 * the end deletes the stored name, so the owner's growth re-send, which
 * carries the ORIGINAL stamp, is the first name the device holds again and is
 * taken. Kept beside [ScreenPollLoopTest], which pins that the re-send is read
 * at all once the person is back.
 */
class GroupNameRejoinTest {

    private val owner = ByteArray(32) { 1 }
    private val admin = ByteArray(32) { 2 }
    private val roles = GroupPushRouting.OwnerLookup.Known(owner = owner, admins = listOf(admin))
    private val name = StoredGroupName(name = "Walk group", updatedAtMs = 1_000uL)

    private fun verdict(sender: ByteArray, stored: StoredGroupName?) = groupNameVerdict(
        senderSignatureKey = sender,
        owner = roles,
        incoming = name,
        stored = stored,
        isNewer = { a, b -> a > b },
    )

    @Test
    fun `after the end wiped the name, the owner's re-send with its original stamp is taken`() {
        assertEquals(GroupNameVerdict.ACCEPT_OWNER, verdict(owner, stored = null))
    }

    @Test
    fun `and so is an admin's`() {
        assertEquals(GroupNameVerdict.ACCEPT_ADMIN, verdict(admin, stored = null))
    }

    @Test
    fun `a second copy of the same re-send is identical, not refused`() {
        assertEquals(GroupNameVerdict.ACCEPT_IDENTICAL, verdict(admin, stored = name))
    }
}
