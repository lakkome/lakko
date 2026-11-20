// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0090 — the pure decisions of a publisher pass: which devices of an
 * account are admitted or retired (2.1 / 2.2), which KeyPackage is admitted,
 * whether an owner device acts on a device request (decision B), the
 * admission mark (section 6), the reseal takeover (section 4), the journal
 * reset under a device (2.5), the publisher list per account, the merged
 * admission seeds (open question 4).
 */
class PublisherDevicesTest {
    private fun k(b: Int) = ByteArray(32) { b.toByte() }

    @Test
    fun `a roster device with no leaf is admitted and a leaf whose device left is retired`() {
        val self = k(1)
        val plan = planDeviceReconcile(accountLeaves = listOf(self, k(2), k(9)), roster = listOf(self, k(2), k(3)), self = self)
        assertEquals(listOf(k(3).toList()), plan.admit.map { it.toList() })
        assertEquals(listOf(k(9).toList()), plan.retire.map { it.toList() })
    }

    @Test
    fun `a device never retires its own leaf, and a settled account plans nothing`() {
        val self = k(1)
        assertTrue(planDeviceReconcile(listOf(self), roster = listOf(k(2)), self = self).retire.isEmpty())
        assertTrue(planDeviceReconcile(listOf(self, k(2)), roster = listOf(self, k(2)), self = self).isEmpty)
    }

    @Test
    fun `a KeyPackage is admitted only when the directory lists it under that device and it names the account`() {
        val device = k(3)
        val account = k(7)
        val kp = byteArrayOf(1, 2, 3)
        val published = listOf(device to kp)
        assertArrayEquals(kp, keyPackageToAdmit(device, account, published, { device }, { account }))
        // Its leaf is another key.
        assertNull(keyPackageToAdmit(device, account, published, { k(4) }, { account }))
        // Bound to another account.
        assertNull(keyPackageToAdmit(device, account, published, { device }, { k(8) }))
        // Not published under that device id.
        assertNull(keyPackageToAdmit(k(5), account, published, { k(5) }, { account }))
        // A reader that throws admits nothing.
        assertNull(keyPackageToAdmit(device, account, published, { error("bad") }, { account }))
    }

    @Test
    fun `an owner device admits a co-publisher's device only for its own account`() {
        val channel = ByteArray(16) { 1 }
        val owner = k(1)
        val copub = k(2)
        val publishers = listOf(owner, copub)
        fun admit(sender: ByteArray?, kpAccount: ByteArray?, isOwnerDevice: Boolean = true, request: ByteArray = channel) =
            deviceRequestVerdict(isOwnerDevice, request, channel, sender, owner, publishers, true, kpAccount, null)
        assertEquals(DeviceRequestVerdict.ADMIT, admit(copub, copub))
        // A KeyPackage bound to someone else.
        assertEquals(DeviceRequestVerdict.IGNORE, admit(copub, k(9)))
        // Not a current publisher.
        assertEquals(DeviceRequestVerdict.IGNORE, admit(k(9), k(9)))
        // The owner's own devices come from its DeviceGroup roster, never a request.
        assertEquals(DeviceRequestVerdict.IGNORE, admit(owner, owner))
        // Only an owner device acts (decision B), and only for this channel.
        assertEquals(DeviceRequestVerdict.IGNORE, admit(copub, copub, isOwnerDevice = false))
        assertEquals(DeviceRequestVerdict.IGNORE, admit(copub, copub, request = ByteArray(16) { 2 }))
        assertEquals(DeviceRequestVerdict.IGNORE, admit(null, copub))
    }

    @Test
    fun `a retire request is applied only to a leaf of the sender's account`() {
        val channel = ByteArray(16) { 1 }
        val owner = k(1)
        val copub = k(2)
        fun retire(sender: ByteArray, leafAccount: ByteArray?) =
            deviceRequestVerdict(true, channel, channel, sender, owner, listOf(owner, copub), false, null, leafAccount)
        assertEquals(DeviceRequestVerdict.RETIRE, retire(copub, copub))
        // The owner's own forgotten device asks too (2.4).
        assertEquals(DeviceRequestVerdict.RETIRE, retire(owner, owner))
        assertEquals(DeviceRequestVerdict.IGNORE, retire(copub, owner))
        assertEquals(DeviceRequestVerdict.IGNORE, retire(copub, null))
    }

    @Test
    fun `an admission mark counts only from the owner's account`() {
        // Security pass — a co-publisher's mark is ignored, however high.
        val owner = k(1)
        val copub = k(2)
        val fromCopub = admissionMarkVerdict(held = 4, received = 9, sender = copub, ownerKey = owner, seenThrough = 100, pendingHeld = 0)
        assertNull(fromCopub.apply)
        assertNull(fromCopub.pending)
        assertNull(admissionMarkVerdict(4, 9, null, owner, 100, 0).apply)
        assertEquals(9L, admissionMarkVerdict(4, 9, owner, owner, 100, 0).apply)
        // Never lowered.
        assertNull(admissionMarkVerdict(9, 4, owner, owner, 100, 0).apply)
    }

    @Test
    fun `an owner mark above the rows this device has read is clamped and the rest kept pending`() {
        val owner = k(1)
        val clamped = admissionMarkVerdict(held = 4, received = 50, sender = owner, ownerKey = owner, seenThrough = 20, pendingHeld = 0)
        assertEquals(20L, clamped.apply)
        assertEquals(50L, clamped.pending)
        // Once this device has read further, the pending part counts.
        val later = pendingAdmissionMark(held = 20, target = 50, seenThrough = 60)
        assertEquals(50L, later.apply)
        assertNull(later.pending)
        // Read nothing new: nothing moves, the pending part stays.
        val still = pendingAdmissionMark(held = 20, target = 50, seenThrough = 20)
        assertNull(still.apply)
        assertEquals(50L, still.pending)
    }

    @Test
    fun `an unfinished reseal is taken over after the wait, never while this device's own commit runs`() {
        val t0 = 1_000_000L
        assertFalse(resealTakeoverDue(5, 4, t0, t0 + UNFINISHED_RESEAL_WAIT_MS - 1, ownCommitPending = false))
        assertTrue(resealTakeoverDue(5, 4, t0, t0 + UNFINISHED_RESEAL_WAIT_MS, ownCommitPending = false))
        assertFalse(resealTakeoverDue(5, 4, t0, t0 + UNFINISHED_RESEAL_WAIT_MS, ownCommitPending = true))
        assertFalse(resealTakeoverDue(5, 5, t0, t0 + UNFINISHED_RESEAL_WAIT_MS, ownCommitPending = false))
        assertFalse(resealTakeoverDue(5, 4, null, t0, ownCommitPending = false))
    }

    @Test
    fun `a journal reset under a device is read from repeated refusals or a DS behind it`() {
        assertFalse(journalResetUnder(RESET_REFUSALS - 1, null, 4uL))
        assertTrue(journalResetUnder(RESET_REFUSALS, null, 4uL))
        assertTrue(journalResetUnder(0, 1uL, 4uL))
        assertFalse(journalResetUnder(0, 4uL, 4uL))
        assertFalse(journalResetUnder(0, null, 4uL))
    }

    @Test
    fun `the rotation counts the posts listed under the current epoch, at least what it kept`() {
        assertEquals(2u, postsUnderEpoch(listOf(3u, 3u, 2u, null), current = 3u, kept = 0u))
        assertEquals(5u, postsUnderEpoch(listOf(3u), current = 3u, kept = 5u))
    }

    @Test
    fun `the jitter bound is one minute`() {
        assertEquals(60_000L, ROTATION_JITTER_MAX_MS)
    }

    @Test
    fun `two seeds of one admission version are both kept, a seed in both lists once`() {
        val a = ByteArray(32) { 1 }
        val b = ByteArray(32) { 2 }
        val c = ByteArray(32) { 3 }
        val merged = mergeAdmissionSeeds(
            local = listOf(1u.toUShort() to a, 0u.toUShort() to c),
            register = listOf(1u.toUShort() to b, 1u.toUShort() to a.copyOf()),
        )
        assertEquals(listOf(0, 1, 1), merged.map { it.first.toInt() })
        assertArrayEquals(c, merged[0].second)
        assertArrayEquals(a, merged[1].second)
        assertArrayEquals(b, merged[2].second)
    }

    @Test
    fun `every seed of a version gets its own sent mark`() {
        val v = 2u.toUShort()
        val one = admissionSeedMark(v, ByteArray(32) { 1 })
        val two = admissionSeedMark(v, ByteArray(32) { 2 })
        assertTrue(one.startsWith("2:"))
        assertFalse(one == two)
        assertEquals(one, admissionSeedMark(v, ByteArray(32) { 1 }))
    }

    @Test
    fun `publishers are one row per account, the owner named by its account key`() {
        val owner = k(1)
        val copub = k(2)
        val members = listOf(
            uniffi.lakko_ffi_uniffi.GroupMember(0u, k(10), owner),
            uniffi.lakko_ffi_uniffi.GroupMember(1u, k(11), copub),
            // A second device of each account.
            uniffi.lakko_ffi_uniffi.GroupMember(2u, k(12), owner),
            uniffi.lakko_ffi_uniffi.GroupMember(3u, k(13), copub),
        )
        val rows = publishersOf(members, ownerKey = owner, ownKey = copub)
        assertEquals(2, rows.size)
        assertTrue(rows[0].owner && rows[0].accountKey.contentEquals(owner))
        assertTrue(rows[1].self && !rows[1].owner)
    }

    @Test
    fun `the owner is named even when a co-publisher's device holds leaf 0`() {
        // A retired founding leaf is reused by the next member added.
        val owner = k(1)
        val copub = k(2)
        val members = listOf(
            uniffi.lakko_ffi_uniffi.GroupMember(0u, k(11), copub),
            uniffi.lakko_ffi_uniffi.GroupMember(1u, k(10), owner),
        )
        val rows = publishersOf(members, ownerKey = owner, ownKey = owner)
        assertTrue(rows.first { it.owner }.accountKey.contentEquals(owner))
        assertFalse(rows.first { it.accountKey.contentEquals(copub) }.owner)
    }

    @Test
    fun `a pending publisher request written before ADR-0090 carries no owner key`() {
        val old = """[{"channelIdHex":"c","groupIdHex":"g","joinerKeyB64":"k","name":"n","requestedAtMs":1}]"""
        val back = PendingPublisherRequests.decode(old)
        assertEquals(1, back.size)
        assertNull(back[0].ownerAccountKeyB64)
        val now = PendingPublisherRequests.Request("c", "g", "k", "n", 1L, ownerAccountKeyB64 = "o")
        assertEquals("o", PendingPublisherRequests.decode(PendingPublisherRequests.encode(listOf(now)))[0].ownerAccountKeyB64)
    }

    @Test
    fun `a refounded group's epoch serves the channel epoch through its offset`() {
        // The service at 7, the new group at 0: offset 7, and each later
        // commit moves both by one.
        val offset = offsetFor(7u, 0uL)
        assertEquals(7L, offset)
        assertEquals(7u, channelEpochFor(0uL, offset))
        assertEquals(9u, channelEpochFor(2uL, offset))
        // Never re-founded: the two are one number.
        assertEquals(4u, channelEpochFor(4uL, 0L))
        assertNull(channelEpochFor(0uL, -1L))
    }

    @Test
    fun `a co-publisher device joins only under a recorded owner key, never one off the Welcome`() {
        val registered = k(1)
        val recorded = k(2)
        assertArrayEquals(registered, coPublisherWelcomeOwnerKey(registered, recorded))
        assertArrayEquals(recorded, coPublisherWelcomeOwnerKey(ByteArray(0), recorded))
        // Nothing recorded: no join (the founder of the Welcome is never asked).
        assertNull(coPublisherWelcomeOwnerKey(ByteArray(0), null))
        assertNull(coPublisherWelcomeOwnerKey(null, ByteArray(16)))
    }

    @Test
    fun `decision A founds again only while the DeviceGroup is this device alone`() {
        val self = k(1)
        assertTrue(refoundAllowed(listOf(self), self))
        assertFalse(refoundAllowed(listOf(self, k(2)), self))
        assertFalse(refoundAllowed(listOf(k(2)), self))
        assertFalse(refoundAllowed(emptyList(), self))
    }

    @Test
    fun `a re-founded private channel with no admission seed reseals only to the account's own key`() {
        val own = setOf("AA".repeat(32))
        assertEquals(RefoundReseal.Keys(listOf("aa".repeat(32))), refoundResealPlan(null, public = false, ownKeys = own))
        // Only a channel that was never private reseals to every row.
        assertEquals(RefoundReseal.EveryRow, refoundResealPlan(null, public = true, ownKeys = own))
        // Tickets this device can open: those rows and the own key.
        assertEquals(
            RefoundReseal.Keys(listOf("bb".repeat(32), "aa".repeat(32))),
            refoundResealPlan(listOf("bb".repeat(32)), public = false, ownKeys = own),
        )
    }

    @Test
    fun `a held group is replaced only by a Welcome for it from a device of the owner`() {
        val group = k(5)
        val owner = k(1)
        assertTrue(welcomeReplacesHeldGroup(group, group, owner, owner))
        // Another account's device.
        assertFalse(welcomeReplacesHeldGroup(group, group, k(2), owner))
        // Another group.
        assertFalse(welcomeReplacesHeldGroup(k(6), group, owner, owner))
        // No recorded owner key.
        assertFalse(welcomeReplacesHeldGroup(group, group, ByteArray(0), ByteArray(0)))
    }

    @Test
    fun `a suspected reset keeps the group and only stops posting`() {
        // Security pass — refused reads and a lower DS epoch are not
        // authenticated: the suspected-reset path suspends and never deletes.
        val loop = java.io.File("src/main/kotlin/me/lakko/app/channels/ChannelPublisherLoop.kt").readText()
        val body = loop.substringAfter("private suspend fun becomeReader(").substringBefore("\n    }\n")
        assertTrue(body.contains("ChannelSuspensions.suspend("))
        assertTrue(body.contains("dropPublisherKeys("))
        assertFalse(body.contains("delete"))
        assertFalse(body.contains("store.remove"))
        // The only deletion of a held group is behind the owner-signed check.
        val co = java.io.File("src/main/kotlin/me/lakko/app/channels/CoPublisher.kt").readText()
        val join = co.substringAfter("private suspend fun joinReplacingHeld(").substringBefore("    /** ADR-0090 decision A")
        assertTrue(join.indexOf("welcomeReplacesHeldGroup(") in 0 until join.indexOf("held.delete(device)"))
    }
}
