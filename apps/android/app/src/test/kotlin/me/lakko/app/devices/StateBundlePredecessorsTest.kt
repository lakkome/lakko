// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import java.io.IOException
import java.util.Base64
import me.lakko.app.auth.OwnPredecessor
import me.lakko.app.auth.OwnPredecessorStore
import me.lakko.app.auth.SealedBlobStorage
import me.lakko.app.auth.base64Encode
import me.lakko.app.contacts.InMemoryProvenLinkStore
import me.lakko.app.contacts.ProvenLink
import me.lakko.app.contacts.currentKeyOf
import me.lakko.app.contacts.earliestKeyOf
import me.lakko.app.contacts.keysOfPerson
import me.lakko.app.reach.PREDECESSOR_MAILBOX_READ_MS
import me.lakko.app.reach.predecessorMailboxIsRead
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0066 - a device paired AFTER a rotation of the account keys: what the
 * pairing state bundle carries of the account's earlier keys and of each
 * contact's proven predecessors, and what the receiving device stores.
 *
 * The public key of a seed is the native core's (`Identity.fromSeed`); here
 * it is a stand-in function, the same on both sides, so the rule is driven
 * without the native library.
 */
class StateBundlePredecessorsTest {

    private class MapBlobs : SealedBlobStorage {
        val map = mutableMapOf<String, ByteArray>()
        var failWrites = false
        override fun read(name: String): ByteArray? = map[name]?.copyOf()
        override fun write(name: String, plaintext: ByteArray) {
            if (failWrites) throw IOException("disk")
            map[name] = plaintext.copyOf()
        }
        override fun delete(name: String) {
            map.remove(name)
        }
    }

    private val account = base64Encode(ByteArray(16) { 7 })

    private fun seed(tag: Int) = ByteArray(64) { (tag * 16 + it % 16).toByte() }

    /** Stand-in for `Identity.fromSeed(seed).signaturePublicKey()`. */
    private val pubkeyOf: (ByteArray) -> ByteArray = { s -> ByteArray(32) { i -> (s[i].toInt() xor 0x5A).toByte() } }

    private fun key(tag: Int) = pubkeyOf(seed(tag))
    private fun keyB64(tag: Int) = base64Encode(key(tag))

    /** The sending device: it lived through two rotations (keys 1 -> 2 -> 3,
     *  3 current), adopted at 1_000 and 2_000. */
    private fun senderStore(): OwnPredecessorStore = OwnPredecessorStore(MapBlobs()).also {
        assertTrue(it.add(account, OwnPredecessor(key(1), seed(1), 1_000L, mailboxCursor = 44)))
        assertTrue(it.add(account, OwnPredecessor(key(2), seed(2), 2_000L, mailboxCursor = 55)))
    }

    private fun carriedBySender(): List<StateBundle.CarriedOwnPredecessor> {
        val bundle = StateBundle.Bundle(
            contacts = emptyList(),
            pins = emptyList(),
            knownGroupIdsB64 = emptyList(),
            ownPredecessors = StateBundle.carriedOwnPredecessors(senderStore().load(account)),
        )
        // Through the wire, as the receiving device gets it.
        return StateBundle.decode(StateBundle.encode(bundle))!!.ownPredecessors
    }

    @Test
    fun `the sender carries its earlier keys oldest first with their adoption times`() {
        val carried = carriedBySender()
        assertEquals(listOf(1_000L, 2_000L), carried.map { it.adoptedAtMs })
        assertArrayEquals(seed(1), Base64.getDecoder().decode(carried[0].seedB64))
        assertArrayEquals(seed(2), Base64.getDecoder().decode(carried[1].seedB64))
    }

    @Test
    fun `an account that never rotated carries nothing and the receiver stores nothing`() {
        assertTrue(StateBundle.carriedOwnPredecessors(emptyList()).isEmpty())
        val blobs = MapBlobs()
        val links = InMemoryProvenLinkStore()
        val receipt = StateBundle.receiveOwnPredecessors(
            OwnPredecessorStore(blobs), links, account, emptyList(), key(3), pubkeyOf,
        )!!
        assertEquals(0, receipt.held)
        assertFalse(receipt.conversationKeyChanged)
        assertTrue(blobs.map.isEmpty())
        assertTrue(links.load(account).isEmpty())
    }

    @Test
    fun `the receiver holds the earlier keys as a kept device that adopted would`() {
        val store = OwnPredecessorStore(MapBlobs())
        val links = InMemoryProvenLinkStore()
        val receipt = StateBundle.receiveOwnPredecessors(
            store, links, account, carriedBySender(), currentAccountKey = key(3), pubkeyOfSeed = pubkeyOf, nowMs = 9_000L,
        )!!
        assertEquals(2, receipt.held)
        assertTrue(receipt.conversationKeyChanged)

        val rows = store.load(account)
        assertEquals(2, rows.size)
        assertArrayEquals(key(1), rows[0].signaturePubkey)
        assertArrayEquals(seed(1), rows[0].phraseSeed)
        assertArrayEquals(key(2), rows[1].signaturePubkey)
        assertArrayEquals(seed(2), rows[1].phraseSeed)
        // The window of each mailbox starts when the SENDER adopted, not now.
        assertEquals(listOf(1_000L, 2_000L), rows.map { it.supersededAtMs })
        // The new device reads each old mailbox from the start: the sender's
        // cursor is not its own.
        assertEquals(listOf(0L, 0L), rows.map { it.mailboxCursor })

        // The own-key resolver chain: every earlier key leads to the current.
        val held = links.load(account)
        assertEquals(keyB64(3), currentKeyOf(keyB64(1), held))
        assertEquals(keyB64(3), currentKeyOf(keyB64(2), held))
        assertEquals(keyB64(1), earliestKeyOf(keyB64(3), held))
        assertTrue(held.all { it.accountIdB64 == account })
    }

    @Test
    fun `a device paired after a rotation derives the sender's conversation account key`() {
        val sender = senderStore()
        val receiver = OwnPredecessorStore(MapBlobs())
        // Before the bundle the new device knows only the current key.
        assertNull(receiver.conversationSeed(account))
        StateBundle.receiveOwnPredecessors(
            receiver, InMemoryProvenLinkStore(), account, carriedBySender(), key(3), pubkeyOf,
        )
        val senderSeed = sender.conversationSeed(account)
        val receiverSeed = receiver.conversationSeed(account)
        assertNotNull(senderSeed)
        assertArrayEquals(senderSeed, receiverSeed)
        // The key both bind their conversation leaves under.
        assertArrayEquals(pubkeyOf(senderSeed!!), pubkeyOf(receiverSeed!!))
        assertArrayEquals(key(1), pubkeyOf(receiverSeed))
    }

    @Test
    fun `each old mailbox keeps its own thirty days counted from the sender's adoption`() {
        val store = OwnPredecessorStore(MapBlobs())
        StateBundle.receiveOwnPredecessors(store, InMemoryProvenLinkStore(), account, carriedBySender(), key(3), pubkeyOf)
        val rows = store.load(account)
        val justInsideFirst = 1_000L + PREDECESSOR_MAILBOX_READ_MS - 1
        assertTrue(predecessorMailboxIsRead(rows[0].supersededAtMs, justInsideFirst))
        assertTrue(predecessorMailboxIsRead(rows[1].supersededAtMs, justInsideFirst))
        // Past the first key's window the second key's still runs.
        val pastFirst = 1_000L + PREDECESSOR_MAILBOX_READ_MS
        assertFalse(predecessorMailboxIsRead(rows[0].supersededAtMs, pastFirst))
        assertTrue(predecessorMailboxIsRead(rows[1].supersededAtMs, pastFirst))
        assertFalse(predecessorMailboxIsRead(rows[1].supersededAtMs, 2_000L + PREDECESSOR_MAILBOX_READ_MS))
    }

    @Test
    fun `a device that does not hold its seed yet records the last step when the chat layer loads`() {
        val store = OwnPredecessorStore(MapBlobs())
        val links = InMemoryProvenLinkStore()
        StateBundle.receiveOwnPredecessors(store, links, account, carriedBySender(), currentAccountKey = null, pubkeyOfSeed = pubkeyOf)
        // Only the step between the two earlier keys is known so far.
        assertEquals(keyB64(2), currentKeyOf(keyB64(1), links.load(account)))
        // What the chat layer records at load, from the stored keys.
        assertEquals(
            listOf(keyB64(1) to keyB64(2), keyB64(2) to keyB64(3)),
            StateBundle.ownChainLinks(listOf(keyB64(1), keyB64(2)), keyB64(3)),
        )
        assertEquals(listOf(keyB64(1) to keyB64(2)), StateBundle.ownChainLinks(listOf(keyB64(1), keyB64(2)), null))
        assertTrue(StateBundle.ownChainLinks(emptyList(), keyB64(3)).isEmpty())
        // A current key that IS the latest stored key adds no step.
        assertEquals(listOf(keyB64(1) to keyB64(2)), StateBundle.ownChainLinks(listOf(keyB64(1), keyB64(2)), keyB64(2)))
    }

    @Test
    fun `receiving the same bundle twice changes nothing, and a row this device holds is kept`() {
        val store = OwnPredecessorStore(MapBlobs())
        val links = InMemoryProvenLinkStore()
        StateBundle.receiveOwnPredecessors(store, links, account, carriedBySender(), key(3), pubkeyOf)
        store.advanceMailboxCursor(account, key(1), 12)
        val again = StateBundle.receiveOwnPredecessors(store, links, account, carriedBySender(), key(3), pubkeyOf)!!
        assertFalse(again.conversationKeyChanged)
        assertEquals(2, again.held)
        assertEquals(listOf(12L, 0L), store.load(account).map { it.mailboxCursor })
        assertEquals(2, links.load(account).size)
    }

    @Test
    fun `a key this device adopted itself stays after the carried ones`() {
        val held = listOf(OwnPredecessor(key(3), seed(3), 3_000L, mailboxCursor = 7))
        val carried = listOf(OwnPredecessor(key(1), seed(1), 1_000L), OwnPredecessor(key(2), seed(2), 2_000L))
        val merged = StateBundle.mergedOwnPredecessors(held, carried)
        assertEquals(listOf(1_000L, 2_000L, 3_000L), merged.map { it.supersededAtMs })
        assertEquals(7L, merged.last().mailboxCursor)
        // Past the bound the earliest (the conversation key) is never dropped.
        val many = (1..9).map { OwnPredecessor(key(it), seed(it), it.toLong()) }
        val bounded = StateBundle.mergedOwnPredecessors(many.takeLast(1), many.dropLast(1))
        assertEquals(8, bounded.size)
        assertArrayEquals(key(1), bounded.first().signaturePubkey)
        assertArrayEquals(key(9), bounded.last().signaturePubkey)
    }

    @Test
    fun `a seed that is not 64 bytes or a store that cannot be written stores nothing`() {
        val links = InMemoryProvenLinkStore()
        val blobs = MapBlobs()
        val bad = listOf(StateBundle.CarriedOwnPredecessor(Base64.getEncoder().encodeToString(ByteArray(32)), 1L))
        assertNull(StateBundle.receiveOwnPredecessors(OwnPredecessorStore(blobs), links, account, bad, key(3), pubkeyOf))
        assertTrue(blobs.map.isEmpty())
        blobs.failWrites = true
        assertNull(
            StateBundle.receiveOwnPredecessors(OwnPredecessorStore(blobs), links, account, carriedBySender(), key(3), pubkeyOf),
        )
        assertTrue(links.load(account).isEmpty())
    }

    // ---- contacts ------------------------------------------------------------

    private val contact = base64Encode(ByteArray(16) { 2 })

    @Test
    fun `the sender carries a contact's proven predecessors oldest first`() {
        val links = listOf(
            ProvenLink(contact, "k1", "k2", 1L),
            ProvenLink(contact, "k2", "k3", 2L),
            ProvenLink("someone-else", "x1", "x2", 3L),
        )
        assertEquals(listOf("k1", "k2"), StateBundle.provenPredecessorsOf("k3", links))
        assertEquals(emptyList<String>(), StateBundle.provenPredecessorsOf("k1", links))
        assertEquals(emptyList<String>(), StateBundle.provenPredecessorsOf("unknown", links))
        // A chain past the bound keeps the steps that end at the pinned key.
        val long = (1..10).map { ProvenLink(contact, "c$it", "c${it + 1}", it.toLong()) }
        assertEquals((3..10).map { "c$it" }, StateBundle.provenPredecessorsOf("c11", long))
    }

    @Test
    fun `the receiver records a contact's predecessors as proven`() {
        val links = InMemoryProvenLinkStore()
        val pin = StateBundle.CarriedPin(contact, "k3", 5L, provenPredecessorsB64 = listOf("k1", "k2"))
        assertEquals(2, StateBundle.receiveContactPredecessors(links, account, pin, heldPinnedKeyB64 = "k3", nowMs = 77L))
        val held = links.load(account)
        assertEquals(listOf("k1", "k2", "k3"), keysOfPerson("k1", held))
        assertTrue(held.all { it.accountIdB64 == contact && it.provenAtMs == 77L })
        // Again: nothing new.
        assertEquals(0, StateBundle.receiveContactPredecessors(links, account, pin, heldPinnedKeyB64 = "k3"))
    }

    @Test
    fun `a pin this device did not take records no predecessors`() {
        val links = InMemoryProvenLinkStore()
        val pin = StateBundle.CarriedPin(contact, "k3", 5L, provenPredecessorsB64 = listOf("k1", "k2"))
        // The monotone setter kept another key (or none) for the account.
        assertEquals(0, StateBundle.receiveContactPredecessors(links, account, pin, heldPinnedKeyB64 = "k9"))
        assertEquals(0, StateBundle.receiveContactPredecessors(links, account, pin, heldPinnedKeyB64 = null))
        assertTrue(links.load(account).isEmpty())
        // No predecessors carried: nothing to record.
        val plain = StateBundle.CarriedPin(contact, "k3", 5L)
        assertEquals(0, StateBundle.receiveContactPredecessors(links, account, plain, heldPinnedKeyB64 = "k3"))
    }
}
