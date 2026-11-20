// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Test

/**
 * Walk of 2026-11-14, defect 2 — an owner device that returns after another
 * device of the account retired its leaf still holds its old PublisherGroup
 * and keys. The owner device's Welcome that admits it again must reach it:
 * the target list is the account's owned / co-published channels this device
 * lists, held or not (the mailbox loop used to skip every channel it held
 * keys for, so the replace path never ran for exactly that device), and the
 * one channel touched is the one whose group id the Welcome names.
 */
class PublisherWelcomeTargetsTest {
    private fun register(
        id: String,
        role: ChannelAccountRole,
        subscribed: Boolean = true,
        groupId: ByteArray = ByteArray(0),
    ) = ChannelRegisterView(
        channelIdHex = id,
        hlc = 1uL,
        subscribed = subscribed,
        role = role,
        subscriberSecret = ByteArray(32),
        readKey = ByteArray(0),
        publisherGroupId = groupId,
        name = "",
        handle = "",
    )

    private val owned = "11".repeat(16)
    private val coPublished = "22".repeat(16)
    private val subscribed = "33".repeat(16)
    private val left = "44".repeat(16)
    private val unlisted = "55".repeat(16)

    @Test
    fun `every owned or co-published listed channel is a target, held keys or not`() {
        val registers = listOf(
            register(owned, ChannelAccountRole.OWNER),
            register(coPublished, ChannelAccountRole.CO_PUBLISHER, groupId = ByteArray(32) { 2 }),
            register(subscribed, ChannelAccountRole.SUBSCRIBER),
            register(left, ChannelAccountRole.OWNER, subscribed = false),
            register(unlisted, ChannelAccountRole.OWNER),
        )
        // The device lists (and, in the walk, holds keys for) the owned one.
        val listed = setOf(owned.uppercase(), coPublished, subscribed, left)
        assertEquals(
            listOf(owned, coPublished),
            publisherWelcomeTargets(registers, listed).map { it.channelIdHex },
        )
    }

    @Test
    fun `only the channel whose group the Welcome names is touched`() {
        val a = register(owned, ChannelAccountRole.OWNER)
        val b = register(coPublished, ChannelAccountRole.CO_PUBLISHER, groupId = ByteArray(32) { 2 })
        val ownedGroup = ByteArray(32) { 1 }
        val expected: (ChannelRegisterView) -> ByteArray? = { t ->
            if (t.role == ChannelAccountRole.OWNER) ownedGroup else t.publisherGroupId
        }
        assertSame(a, publisherWelcomeTarget(listOf(a, b), ByteArray(32) { 1 }, expected))
        assertSame(b, publisherWelcomeTarget(listOf(a, b), ByteArray(32) { 2 }, expected))
        // A conversation Welcome, or one this device cannot read: none.
        assertNull(publisherWelcomeTarget(listOf(a, b), ByteArray(32) { 9 }, expected))
        assertNull(publisherWelcomeTarget(listOf(a, b), null, expected))
        // An id that cannot be computed matches nothing.
        assertNull(publisherWelcomeTarget(listOf(a), ByteArray(32) { 1 }) { error("no seed") })
    }

    @Test
    fun `waits for a Welcome as an owner device with siblings, reads alone, ends only when the account was removed`() {
        // The same table as the web `inactivePublisherGroupVerdict` test.
        fun v(owner: Boolean, leaf: Boolean?, others: Boolean?) = inactivePublisherGroupVerdict(owner, leaf, others)
        assertEquals(InactiveGroupVerdict.WAIT_FOR_WELCOME, v(owner = true, leaf = true, others = true))
        assertEquals(InactiveGroupVerdict.READER, v(owner = true, leaf = null, others = false))
        // A co-publisher's device retired while its account still publishes:
        // it waits, it does not end the channel.
        assertEquals(InactiveGroupVerdict.WAIT_FOR_WELCOME, v(owner = false, leaf = true, others = true))
        assertEquals(InactiveGroupVerdict.REMOVED, v(owner = false, leaf = false, others = true))
        assertEquals(InactiveGroupVerdict.REMOVED, v(owner = false, leaf = null, others = true))
        assertEquals(InactiveGroupVerdict.REMOVED, v(owner = false, leaf = true, others = false))
        assertEquals(InactiveGroupVerdict.UNDECIDED, v(owner = true, leaf = null, others = null))
        assertEquals(InactiveGroupVerdict.UNDECIDED, v(owner = true, leaf = false, others = null))
        assertEquals(InactiveGroupVerdict.UNDECIDED, v(owner = false, leaf = false, others = null))
    }

    @Test
    fun `an owner device whose account still holds a leaf waits even when its own roster shows it alone`() {
        // Security pass F4 (2026-11-15) - the commit that removed this owner
        // device's leaf came from another device of the account (peer gate
        // C-90): a leaf of the owner account left in the group is another own
        // device, whatever a stale DeviceGroup roster (a re-founded one) says.
        // It waits for that device's Welcome; it never takes the channel alone.
        fun v(leaf: Boolean?, others: Boolean?) = inactivePublisherGroupVerdict(ownerDevice = true, ownAccountHasLeaf = leaf, otherOwnDevices = others)
        assertEquals(InactiveGroupVerdict.WAIT_FOR_WELCOME, v(leaf = true, others = false))
        assertEquals(InactiveGroupVerdict.WAIT_FOR_WELCOME, v(leaf = true, others = null))
        // No leaf of the account left and the roster alone: it reads.
        assertEquals(InactiveGroupVerdict.READER, v(leaf = false, others = false))
    }

    @Test
    fun `an evicted group does not block the entry from the saved copy or a re-founding`() {
        // Owner device alone (READER): its held group is inactive, so the
        // entry is wanted and the re-founding is not skipped.
        assertEquals(false, holdsLiveGroup(groupHeld = true, groupActive = false))
        assertEquals(
            true,
            entryWanted(
                ownerRole = true,
                subscribed = true,
                holdsKeys = false,
                holdsGroup = holdsLiveGroup(groupHeld = true, groupActive = false),
                suspended = false,
                orphaned = false,
            ),
        )
        // A live group still counts as held, and no group is not held.
        assertEquals(true, holdsLiveGroup(groupHeld = true, groupActive = true))
        assertEquals(false, holdsLiveGroup(groupHeld = false, groupActive = false))
        // An owner device that waits for a Welcome is suspended: no entry.
        assertEquals(
            false,
            entryWanted(
                ownerRole = true,
                subscribed = true,
                holdsKeys = false,
                holdsGroup = false,
                suspended = true,
                orphaned = false,
            ),
        )
    }
}
