// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0058 — the pure half of the reach rules: which keys the owner's mailbox
 * holds, what a registration sends, whose grants are kept (C-68), and which
 * held grant signs a write. Host JVM only; no native library.
 */
class ReachRulesTest {

    private val now = 1_000_000L

    private fun inputs(
        versions: Map<ReachSlot, Int> = emptyMap(),
        joined: Set<String> = emptySet(),
        visible: Set<String> = emptySet(),
        channels: Set<String> = emptySet(),
        fof: Boolean = false,
        links: Map<String, Long?> = emptyMap(),
        replies: Map<String, Long> = emptyMap(),
    ) = ReachInputs(versions, joined, visible, channels, fof, links, replies)

    // -----------------------------------------------------------------
    // The desired key set
    // -----------------------------------------------------------------

    @Test
    fun `a new account holds exactly its contact key`() {
        val keys = desiredReachKeys(inputs(), now)
        assertEquals(listOf(DesiredReachKey(ReachSlot(ReachKind.CONTACT, ""), 0, null)), keys)
    }

    @Test
    fun `every joined group carries a member key, and only visible ones a group key`() {
        val keys = desiredReachKeys(inputs(joined = setOf("aa", "bb"), visible = setOf("bb")), now)
        val slots = keys.map { it.slot }.toSet()
        assertTrue(ReachSlot(ReachKind.MEMBER, "aa") in slots)
        assertTrue(ReachSlot(ReachKind.MEMBER, "bb") in slots)
        assertTrue(ReachSlot(ReachKind.GROUP, "bb") in slots)
        assertFalse(ReachSlot(ReachKind.GROUP, "aa") in slots)
    }

    @Test
    fun `a visible group this account left carries no group key`() {
        val keys = desiredReachKeys(inputs(joined = setOf("aa"), visible = setOf("aa", "gone")), now)
        assertFalse(keys.any { it.slot == ReachSlot(ReachKind.GROUP, "gone") })
    }

    @Test
    fun `contacts of contacts adds the fof key`() {
        assertTrue(desiredReachKeys(inputs(fof = true), now).any { it.slot.kind == ReachKind.FOF })
        assertFalse(desiredReachKeys(inputs(fof = false), now).any { it.slot.kind == ReachKind.FOF })
    }

    @Test
    fun `an expired link or reply contributes no key and a live one keeps its expiry`() {
        val keys = desiredReachKeys(
            inputs(
                links = mapOf("11" to now + 5, "22" to now - 1, "33" to null),
                replies = mapOf("r1" to now + 10, "r2" to now),
            ),
            now,
        )
        val link11 = keys.single { it.slot == ReachSlot(ReachKind.LINK, "11") }
        assertEquals(now + 5, link11.expiresAtMs)
        assertNull(keys.single { it.slot == ReachSlot(ReachKind.LINK, "33") }.expiresAtMs)
        assertFalse(keys.any { it.slot == ReachSlot(ReachKind.LINK, "22") })
        assertTrue(keys.any { it.slot == ReachSlot(ReachKind.REPLY, "r1") })
        assertFalse(keys.any { it.slot == ReachSlot(ReachKind.REPLY, "r2") })
    }

    @Test
    fun `a rotated slot is desired at its new version`() {
        val keys = desiredReachKeys(
            inputs(versions = mapOf(ReachSlot(ReachKind.CONTACT, "") to 3)),
            now,
        )
        assertEquals(3, keys.single { it.slot.kind == ReachKind.CONTACT }.version)
    }

    // -----------------------------------------------------------------
    // The registration diff
    // -----------------------------------------------------------------

    @Test
    fun `a key already registered with the same expiry is not re-sent`() {
        val key = DesiredReachKey(ReachSlot(ReachKind.CONTACT, ""), 0, null)
        val diff = diffReachKeys(
            desired = mapOf("pk1" to key),
            registered = mapOf("pk1" to RegisteredReachKey(key.slot, 0, null)),
        )
        assertTrue(diff.isEmpty)
    }

    @Test
    fun `a rotation adds the new key and removes the old one in one update`() {
        val slot = ReachSlot(ReachKind.CONTACT, "")
        val diff = diffReachKeys(
            desired = mapOf("pkNew" to DesiredReachKey(slot, 1, null)),
            registered = mapOf("pkOld" to RegisteredReachKey(slot, 0, null)),
        )
        assertEquals(setOf("pkNew"), diff.add.keys)
        assertEquals(setOf("pkOld"), diff.remove)
    }

    @Test
    fun `a changed expiry re-adds the key`() {
        val slot = ReachSlot(ReachKind.LINK, "11")
        val diff = diffReachKeys(
            desired = mapOf("pk" to DesiredReachKey(slot, 0, 20)),
            registered = mapOf("pk" to RegisteredReachKey(slot, 0, 10)),
        )
        assertEquals(setOf("pk"), diff.add.keys)
        assertTrue(diff.remove.isEmpty())
    }

    @Test
    fun `a revoked link's key comes off the mailbox`() {
        val diff = diffReachKeys(
            desired = emptyMap(),
            registered = mapOf("pkLink" to RegisteredReachKey(ReachSlot(ReachKind.LINK, "11"), 0, null)),
        )
        assertEquals(setOf("pkLink"), diff.remove)
    }

    // -----------------------------------------------------------------
    // Which received grants are kept (C-68)
    // -----------------------------------------------------------------

    private val self = "5e1f"
    private val peer = "beef"
    private val third = "cafe"

    @Test
    fun `a contact grant is kept only from the other participant of a one-to-one`() {
        assertTrue(acceptReceivedGrant(ReachKind.CONTACT, GrantVenue.ONE_TO_ONE, peer, self, peer, true))
        assertFalse(acceptReceivedGrant(ReachKind.CONTACT, GrantVenue.ONE_TO_ONE, third, self, peer, true))
        assertFalse(acceptReceivedGrant(ReachKind.CONTACT, GrantVenue.GROUP, peer, self, null, true))
        assertFalse(acceptReceivedGrant(ReachKind.CONTACT, GrantVenue.CHANNEL_CARD, peer, self, null, true))
    }

    @Test
    fun `group and member grants are kept only from a member`() {
        assertTrue(acceptReceivedGrant(ReachKind.GROUP, GrantVenue.GROUP, peer, self, null, true))
        assertFalse(acceptReceivedGrant(ReachKind.GROUP, GrantVenue.GROUP, peer, self, null, false))
        assertTrue(acceptReceivedGrant(ReachKind.MEMBER, GrantVenue.GROUP, peer, self, null, true))
        assertTrue(acceptReceivedGrant(ReachKind.MEMBER, GrantVenue.ONE_TO_ONE, peer, self, peer, true))
        assertFalse(acceptReceivedGrant(ReachKind.MEMBER, GrantVenue.GROUP, peer, self, null, false))
        assertFalse(acceptReceivedGrant(ReachKind.GROUP, GrantVenue.ONE_TO_ONE, peer, self, peer, true))
    }

    @Test
    fun `a channel grant is kept only from a member card`() {
        assertTrue(acceptReceivedGrant(ReachKind.CHANNEL, GrantVenue.CHANNEL_CARD, peer, self, null, false))
        assertFalse(acceptReceivedGrant(ReachKind.CHANNEL, GrantVenue.GROUP, peer, self, null, true))
    }

    @Test
    fun `link reply and fof grants never ride a profile`() {
        for (kind in listOf(ReachKind.LINK, ReachKind.REPLY, ReachKind.FOF)) {
            for (venue in GrantVenue.entries) {
                assertFalse(acceptReceivedGrant(kind, venue, peer, self, peer, true))
            }
        }
    }

    @Test
    fun `an unresolved sender or this account itself is refused`() {
        assertFalse(acceptReceivedGrant(ReachKind.MEMBER, GrantVenue.GROUP, null, self, null, true))
        assertFalse(acceptReceivedGrant(ReachKind.MEMBER, GrantVenue.GROUP, "", self, null, true))
        assertFalse(acceptReceivedGrant(ReachKind.CONTACT, GrantVenue.ONE_TO_ONE, self, self, self, true))
    }

    // -----------------------------------------------------------------
    // Which held grant signs a write
    // -----------------------------------------------------------------

    @Test
    fun `a contact request never uses a member key`() {
        val held = listOf(HeldGrantRef(ReachKind.MEMBER, "aa"))
        assertNull(pickWriteGrant(held, WritePurpose.CONTACT_REQUEST))
    }

    @Test
    fun `a contact request uses a group grant, then channel, then fof`() {
        val held = listOf(
            HeldGrantRef(ReachKind.FOF, ""),
            HeldGrantRef(ReachKind.CHANNEL, "cc"),
            HeldGrantRef(ReachKind.GROUP, "aa"),
        )
        assertEquals(HeldGrantRef(ReachKind.GROUP, "aa"), pickWriteGrant(held, WritePurpose.CONTACT_REQUEST))
        assertEquals(
            HeldGrantRef(ReachKind.FOF, ""),
            pickWriteGrant(listOf(HeldGrantRef(ReachKind.FOF, "")), WritePurpose.CONTACT_REQUEST),
        )
    }

    @Test
    fun `a reply prefers the requester's reply key, then their contact key`() {
        val held = listOf(HeldGrantRef(ReachKind.CONTACT, ""), HeldGrantRef(ReachKind.REPLY, ""))
        assertEquals(HeldGrantRef(ReachKind.REPLY, ""), pickWriteGrant(held, WritePurpose.REPLY))
        assertEquals(
            HeldGrantRef(ReachKind.CONTACT, ""),
            pickWriteGrant(listOf(HeldGrantRef(ReachKind.CONTACT, "")), WritePurpose.REPLY),
        )
        assertNull(pickWriteGrant(listOf(HeldGrantRef(ReachKind.GROUP, "aa")), WritePurpose.REPLY))
    }

    @Test
    fun `a Welcome uses the member key of that group before a group key elsewhere`() {
        val held = listOf(
            HeldGrantRef(ReachKind.GROUP, "zz"),
            HeldGrantRef(ReachKind.MEMBER, "aa"),
            HeldGrantRef(ReachKind.MEMBER, "bb"),
        )
        assertEquals(HeldGrantRef(ReachKind.MEMBER, "bb"), pickWriteGrant(held, WritePurpose.WELCOME, "bb"))
        assertEquals(HeldGrantRef(ReachKind.GROUP, "zz"), pickWriteGrant(held, WritePurpose.WELCOME, "cc"))
    }

    @Test
    fun `nothing held means no grant`() {
        for (purpose in WritePurpose.entries) assertNull(pickWriteGrant(emptyList(), purpose, "aa"))
    }

    // -----------------------------------------------------------------
    // Reply lifecycle and hex
    // -----------------------------------------------------------------

    @Test
    fun `a reply key lives seven days and is expired exactly at its expiry`() {
        assertEquals(now + 7L * 24 * 60 * 60 * 1000, replyExpiry(now))
        val replies = mapOf("a" to now, "b" to now + 1)
        assertEquals(setOf("a"), expiredReplies(replies, now))
    }

    @Test
    fun `hex round-trips and refuses malformed input`() {
        val bytes = byteArrayOf(0, 1, 0x7f, -1, -128)
        assertEquals("00017fff80", reachHex(bytes))
        assertArrayEquals(bytes, reachUnhex("00017FFF80"))
        assertNull(reachUnhex("abc"))
        assertNull(reachUnhex("zz"))
    }
}

class ReachShrinkRotationTest {
    @Test
    fun `someone leaving rotates, growth and a torn read do not`() {
        assertTrue(reachRotationOnShrink(listOf("a", "b"), listOf("a"), selfEvicted = false))
        assertFalse(reachRotationOnShrink(listOf("a"), listOf("a", "b"), selfEvicted = false))
        assertFalse(reachRotationOnShrink(listOf("a", "b"), listOf("a", "b"), selfEvicted = false))
        assertFalse(reachRotationOnShrink(null, listOf("a"), selfEvicted = false))
        assertFalse(reachRotationOnShrink(listOf("a", "b"), emptyList(), selfEvicted = false))
    }

    @Test
    fun `a remove and an add in one batch still counts as a shrink`() {
        assertTrue(reachRotationOnShrink(listOf("a", "b"), listOf("a", "c"), selfEvicted = false))
    }

    @Test
    fun `an evicted device does not rotate`() {
        assertFalse(reachRotationOnShrink(listOf("a", "b"), listOf("a"), selfEvicted = true))
    }
}
