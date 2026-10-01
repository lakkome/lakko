// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import java.util.Base64
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * The accepted-contact list belongs to an ACCOUNT, not to the device (the
 * 2026-09-30 live finding: a second account signed in where a first had been
 * listed, messaged and searched the first account's contacts as its own).
 * Host-side over an in-memory backend; run as `:app:testDebugUnitTest`.
 */
class AcceptedContactsStoreTest {

    private class MemoryBackend : AcceptedContactsStore.Backend {
        val map = HashMap<String, String>()
        override fun get(key: String): String? = map[key]
        override fun put(key: String, value: String) {
            map[key] = value
        }
        override fun remove(key: String) {
            map.remove(key)
        }
    }

    private val cara = ByteArray(16) { 0x0C }
    private val eve = ByteArray(16) { 0x0E }
    private val caraB64 = Base64.getEncoder().encodeToString(cara)
    private val eveB64 = Base64.getEncoder().encodeToString(eve)

    private lateinit var backend: MemoryBackend

    private fun contact(pubkeyByte: Int, name: String? = null) = AcceptedContact(
        request = IncomingContactRequest(
            messageId = 1_000L + pubkeyByte,
            fromPubkey = ByteArray(32) { pubkeyByte.toByte() },
            fromAccountId = ByteArray(16) { pubkeyByte.toByte() },
            fromPseudonym = ByteArray(32),
            validAfterMs = 1_700_000_000_000uL,
            intro = null,
            createdAt = "2026-09-30T10:00:00Z",
        ),
        groupIdHex = null,
        displayName = name,
    )

    private fun keysOf(list: List<AcceptedContact>): List<Int> =
        list.map { it.request.fromPubkey[0].toInt() }

    @Before
    fun setUp() {
        backend = MemoryBackend()
        AcceptedContactsStore.unbind()
    }

    @After
    fun tearDown() {
        AcceptedContactsStore.unbind()
    }

    @Test
    fun `a read before any account is bound is empty`() {
        AcceptedContactsStore.saveFor(backend, caraB64, listOf(contact(0x0D)))
        assertNull(AcceptedContactsStore.boundAccountB64)
        assertTrue(AcceptedContactsStore.load(backend).isEmpty())
    }

    @Test
    fun `a write with no account bound is refused and writes nothing`() {
        assertFalse(AcceptedContactsStore.save(backend, listOf(contact(0x0D))))
        assertTrue(backend.map.isEmpty())
    }

    @Test
    fun `two accounts in one store never see each other's contacts`() {
        AcceptedContactsStore.bind(cara)
        assertTrue(AcceptedContactsStore.save(backend, listOf(contact(0x0D, "Dan"))))

        AcceptedContactsStore.bind(eve)
        assertTrue("Eve starts with none of Cara's contacts", AcceptedContactsStore.load(backend).isEmpty())
        assertTrue(AcceptedContactsStore.save(backend, listOf(contact(0x0F))))
        assertEquals(listOf(0x0F), keysOf(AcceptedContactsStore.load(backend)))

        AcceptedContactsStore.bind(cara)
        val caras = AcceptedContactsStore.load(backend)
        assertEquals(listOf(0x0D), keysOf(caras))
        assertEquals("Dan", caras.single().displayName)
        assertEquals(listOf(0x0F), keysOf(AcceptedContactsStore.loadFor(backend, eveB64)))
    }

    @Test
    fun `log out keeps the list for the next login of the same account`() {
        AcceptedContactsStore.bind(cara)
        AcceptedContactsStore.save(backend, listOf(contact(0x0D)))
        AcceptedContactsStore.unbind()
        assertTrue(AcceptedContactsStore.load(backend).isEmpty())
        AcceptedContactsStore.bind(cara)
        assertEquals(listOf(0x0D), keysOf(AcceptedContactsStore.load(backend)))
    }

    @Test
    fun `forget clears only the current account`() {
        AcceptedContactsStore.saveFor(backend, caraB64, listOf(contact(0x0D)))
        AcceptedContactsStore.saveFor(backend, eveB64, listOf(contact(0x0F)))
        AcceptedContactsStore.deleteFor(backend, eveB64)
        assertTrue(AcceptedContactsStore.loadFor(backend, eveB64).isEmpty())
        assertEquals(listOf(0x0D), keysOf(AcceptedContactsStore.loadFor(backend, caraB64)))
    }

    @Test
    fun `an explicit account wins over the bound one`() {
        // A backup restore writes into the RESTORED account's list.
        AcceptedContactsStore.bind(cara)
        AcceptedContactsStore.saveFor(backend, eveB64, listOf(contact(0x0F)))
        assertTrue(AcceptedContactsStore.load(backend).isEmpty())
        assertEquals(listOf(0x0F), keysOf(AcceptedContactsStore.loadFor(backend, eveB64)))
    }

    @Test
    fun `the old device-wide key is never read`() {
        backend.map["accepted.v1"] = AcceptedContactsStore.encode(listOf(contact(0x0D)))
        AcceptedContactsStore.bind(eve)
        assertTrue(AcceptedContactsStore.load(backend).isEmpty())
        AcceptedContactsStore.deleteFor(backend, eveB64)
        assertFalse("forget also drops the retired key", "accepted.v1" in backend.map)
    }

    @Test
    fun `url-safe and standard spellings of one account share one list`() {
        val odd = ByteArray(16) { 0xFB.toByte() }
        val standard = Base64.getEncoder().encodeToString(odd)
        val urlSafe = Base64.getUrlEncoder().withoutPadding().encodeToString(odd)
        AcceptedContactsStore.saveFor(backend, urlSafe, listOf(contact(0x0D)))
        assertEquals(listOf(0x0D), keysOf(AcceptedContactsStore.loadFor(backend, standard)))
        assertEquals(standard, AcceptedContactsStore.canonicalAccountB64(urlSafe))
    }

    @Test
    fun `the codec round-trips every field`() {
        val c = contact(0x0D, "Dan").copy(groupIdHex = "abcd")
        val back = AcceptedContactsStore.decode(AcceptedContactsStore.encode(listOf(c)))!!.single()
        assertEquals(c.request.messageId, back.request.messageId)
        assertTrue(c.request.fromPubkey.contentEquals(back.request.fromPubkey))
        assertTrue(c.request.fromAccountId!!.contentEquals(back.request.fromAccountId!!))
        assertEquals("abcd", back.groupIdHex)
        assertEquals("Dan", back.displayName)
        assertNull(AcceptedContactsStore.decode("not json"))
    }

    @Test
    fun `pins are namespaced by the owning account`() {
        val peer = "cGVlcg=="
        assertFalse(PeerPinStore.pinKey(caraB64, peer) == PeerPinStore.pinKey(eveB64, peer))
        val keys = listOf(
            PeerPinStore.pinKey(caraB64, peer),
            PeerPinStore.pinKey(eveB64, peer),
            "pin-v1:$peer",
        )
        assertEquals(
            listOf(PeerPinStore.pinKey(eveB64, peer), "pin-v1:$peer"),
            PeerPinStore.keysOwnedBy(keys, eveB64),
        )
    }
}
