// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import java.io.IOException
import me.lakko.app.auth.SealedBlobStorage
import me.lakko.app.auth.base64Encode
import me.lakko.app.reach.ADDRESS_RECHECK_INTERVAL_MS
import me.lakko.app.reach.spendRoundOwed
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0074 section 3.5a rule 5 - the durable Sent list (so a re-send after a
 * rotation survives a process death and carries the intro), the marker that
 * retries the spend round, and the re-check throttle of rule 6.
 */
class SentRequestsStoreTest {

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

    private val accountA = base64Encode(ByteArray(16) { 1 })
    private val accountB = base64Encode(ByteArray(16) { 2 })

    private fun sent(id: Long, intro: String = "hello, it is me", to: Int = 9) = SentContactRequest(
        recipientUsername = "bob$id",
        recipientAccountIdB64 = base64Encode(ByteArray(16) { to.toByte() }),
        messageId = id,
        createdAt = "2026-10-11T10:00:00Z",
        introMessage = intro.toByteArray(Charsets.UTF_8),
        sentAtMs = 1_000L + id,
    )

    @Test
    fun `a stored request keeps the recipient, the intro and when it was sent`() {
        val store = SealedSentRequestsStore(MapBlobs())
        val list = listOf(sent(1, intro = "first"), sent(2, intro = ""))
        assertTrue(store.save(accountA, list))
        val back = store.load(accountA)
        assertEquals(list, back)
        assertArrayEquals("first".toByteArray(), back[0].introMessage)
        assertEquals(0, back[1].introMessage.size)
        assertEquals(listOf(1_001L, 1_002L), back.map { it.sentAtMs })
    }

    @Test
    fun `each account has its own list and an emptied list leaves no file`() {
        val blobs = MapBlobs()
        val store = SealedSentRequestsStore(blobs)
        store.save(accountA, listOf(sent(1)))
        store.save(accountB, listOf(sent(2)))
        assertEquals(listOf(1L), store.load(accountA).map { it.messageId })
        assertEquals(listOf(2L), store.load(accountB).map { it.messageId })
        assertTrue(store.save(accountA, emptyList()))
        assertEquals(1, blobs.map.size)
        assertTrue(store.load(accountA).isEmpty())
        store.delete(accountB)
        assertTrue(blobs.map.isEmpty())
    }

    @Test
    fun `the file is named by the account id in hex in either base64 spelling`() {
        val raw = ByteArray(16) { 0xFB.toByte() }
        val std = base64Encode(raw)
        val url = std.replace('+', '-').replace('/', '_')
        assertEquals("fb".repeat(16) + ".sent", SentRequestsFiles.nameOf(std))
        assertEquals(SentRequestsFiles.nameOf(std), SentRequestsFiles.nameOf(url))
        assertNull(SentRequestsFiles.nameOf("not base64!"))
    }

    @Test
    fun `a write that fails says so and unreadable bytes read as an empty list`() {
        val blobs = MapBlobs()
        val store = SealedSentRequestsStore(blobs)
        blobs.failWrites = true
        assertFalse(store.save(accountA, listOf(sent(1))))
        blobs.failWrites = false
        blobs.map[SentRequestsFiles.nameOf(accountA)!!] = "{not json".toByteArray()
        assertTrue(store.load(accountA).isEmpty())
        assertNull(decodeSentRequests("[{\"recipientUsername\":1}]".toByteArray()))
    }

    @Test
    fun `an answered request leaves the stored list`() {
        val store = InMemorySentRequestsStore()
        val list = listOf(sent(1, to = 5), sent(2, to = 6))
        store.save(accountA, list)
        val ack = IncomingContactAck(
            messageId = 40L,
            accepterPubkey = ByteArray(32) { 5 },
            accepterAccountId = ByteArray(16) { 5 },
            accepterX25519Pubkey = ByteArray(32),
            ackToPseudonym = ByteArray(32),
            acceptedAtMs = 1uL,
            createdAt = "",
        )
        // The ack alone keeps it (a contact needs the one-to-one); the
        // Welcome answers it.
        val applied = applyContactAcks(emptyList(), store.load(accountA), listOf(ack))
        store.save(accountA, applied.sent)
        assertEquals(listOf(1L, 2L), store.load(accountA).map { it.messageId })
        val key = java.util.Base64.getEncoder().encodeToString(ByteArray(32) { 5 })
        val answered = applyAnsweringWelcomes(emptyList(), store.load(accountA), mapOf(key to "ab".repeat(32)), 1uL, "")
        store.save(accountA, answered.sent)
        assertEquals(listOf(2L), store.load(accountA).map { it.messageId })
    }

    @Test
    fun `the stored list loads under what was sent before the account was bound`() {
        val stored = listOf(sent(1), sent(2))
        val inMemory = listOf(sent(2), sent(3))
        assertEquals(listOf(1L, 2L, 3L), mergedSentRequests(stored, inMemory).map { it.messageId })
        assertEquals(stored, mergedSentRequests(stored, emptyList()))
    }

    @Test
    fun `the stored list is bounded to the newest entries`() {
        val many = (1L..(MAX_STORED_SENT_REQUESTS + 3L)).map { sent(it) }
        val back = decodeSentRequests(encodeSentRequests(many))!!
        assertEquals(MAX_STORED_SENT_REQUESTS, back.size)
        assertEquals(many.last().messageId, back.last().messageId)
    }

    @Test
    fun `the spend round is owed only under the key the marker names`() {
        assertTrue(spendRoundOwed("aa", "aa"))
        // Nothing moved, or the round already finished.
        assertFalse(spendRoundOwed(null, "aa"))
        // A marker a later move replaced.
        assertFalse(spendRoundOwed("aa", "bb"))
    }

    @Test
    fun `a refused write re-checks one person's address every five minutes`() {
        assertEquals(5L * 60 * 1000, ADDRESS_RECHECK_INTERVAL_MS)
    }

    @Test
    fun `the name a person was found under is kept with the record`() {
        val kept = sent(1).copy(recipientName = "Kaj Ulm", recipientUsername = "")
        val back = decodeSentRequests(encodeSentRequests(listOf(kept)))!!.single()
        assertEquals("Kaj Ulm", back.recipientName)
        assertEquals("", back.recipientUsername)
    }

    @Test
    fun `a record stored before the name was kept still reads`() {
        val old = """[{"recipientUsername":"bob","recipientAccountIdB64":"${accountA}",""" +
            """"messageId":1,"createdAt":"2026-10-11T10:00:00Z"}]"""
        assertNull(decodeSentRequests(old.toByteArray())!!.single().recipientName)
    }
}
