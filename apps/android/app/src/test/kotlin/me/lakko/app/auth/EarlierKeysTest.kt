// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.async
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiBindingVerdict
import uniffi.lakko_ffi_uniffi.FfiOwnLink

/**
 * G-56 (ADR-0074, "Joining after a rotation"): the order of the gate, the link
 * answer, the local record and the lineage guard, on the host JVM. The verdict
 * itself is the core's (`ownBindingVerdict`, its 17 shared vectors are pinned
 * in `lakko-contacts` and, through the FFI, in
 * `OwnBindingVerdictInstrumentedTest`); here a fake stands in for it so the
 * ordering around it can be watched.
 */
class EarlierKeysTest {

    private fun key(tag: Int) = ByteArray(32) { tag.toByte() }

    private val cur = key(9)
    private val k0 = key(1)
    private val k1 = key(2)

    /** A stand-in for the core's rule, close enough to watch the gate's
     *  ordering (the real table is not re-implemented anywhere in the app). */
    private fun fakeVerdict(
        bound: ByteArray?,
        current: ByteArray,
        predecessors: List<ByteArray>,
        link: FfiOwnLink,
        holds: Boolean,
    ): FfiBindingVerdict {
        val target = (link as? FfiOwnLink.Key)?.key ?: predecessors.firstOrNull() ?: current
        fun held(k: ByteArray) = k.contentEquals(current) || predecessors.any { it.contentEquals(k) }
        if (bound != null && predecessors.any { it.contentEquals(bound) }) return FfiBindingVerdict.Keep
        if (bound != null && bound.contentEquals(current) && holds) return FfiBindingVerdict.KeepUnderCurrentKey
        if (bound != null && bound.contentEquals(target)) return FfiBindingVerdict.Keep
        if (link is FfiOwnLink.Unknown) return FfiBindingVerdict.Wait
        if (!held(target)) return FfiBindingVerdict.Wait
        return FfiBindingVerdict.Mint(target)
    }

    private inner class FakePorts(
        var bound: ByteArray? = null,
        var predecessors: MutableList<ByteArray> = mutableListOf(),
        var holds: Boolean = false,
        var link: FfiOwnLink = FfiOwnLink.Absent,
        var recorded: FfiOwnLink = FfiOwnLink.Absent,
        var readThrows: Boolean = false,
    ) : EarlierKeysGatePorts {
        val reads = mutableListOf<String>()
        val records = mutableListOf<FfiOwnLink>()
        val verdictLinks = mutableListOf<FfiOwnLink>()

        override fun boundKey(): ByteArray? = bound

        override fun currentKey(): ByteArray = cur

        override fun predecessorKeys(): List<ByteArray> = predecessors.toList()

        override fun holdsConversations(): Boolean = holds

        override fun verdict(
            boundKey: ByteArray?,
            currentKey: ByteArray,
            predecessorKeys: List<ByteArray>,
            link: FfiOwnLink,
            holdsConversations: Boolean,
        ): FfiBindingVerdict {
            verdictLinks += link
            return fakeVerdict(boundKey, currentKey, predecessorKeys, link, holdsConversations)
        }

        override fun recordedLink(): FfiOwnLink = recorded

        override suspend fun readLink(): FfiOwnLink {
            reads += "read"
            if (readThrows) throw IllegalStateException("offline")
            return link
        }

        override fun recordLink(link: FfiOwnLink) {
            records += link
        }
    }

    // ---- the gate ---------------------------------------------------------

    @Test
    fun `a kept binding lands with no request`() = runTest {
        val ports = FakePorts(bound = k0, predecessors = mutableListOf(k0), holds = true, link = FfiOwnLink.Key(k0))
        val decision = runEarlierKeysGate(ports)
        assertTrue(decision is EarlierKeysDecision.Land)
        assertEquals(FfiBindingVerdict.Keep, (decision as EarlierKeysDecision.Land).verdict)
        assertTrue(ports.reads.isEmpty())
        assertTrue(ports.records.isEmpty())
    }

    @Test
    fun `a link naming a key this device lacks parks it and mints nothing`() = runTest {
        val ports = FakePorts(link = FfiOwnLink.Key(k0))
        val decision = runEarlierKeysGate(ports)
        assertEquals(EarlierKeysDecision.Park, decision)
        assertEquals(listOf("read"), ports.reads)
        // What was read is kept for the chat layer and the lineage guard.
        assertTrue((ports.records.single() as FfiOwnLink.Key).key.contentEquals(k0))
    }

    @Test
    fun `an unreadable bundle parks a device whose binding must be minted`() = runTest {
        val ports = FakePorts(readThrows = true)
        assertEquals(EarlierKeysDecision.Park, runEarlierKeysGate(ports))
        assertEquals(FfiOwnLink.Unknown, ports.records.single())
    }

    @Test
    fun `no link mints under the current key`() = runTest {
        val ports = FakePorts(link = FfiOwnLink.Absent)
        val decision = runEarlierKeysGate(ports) as EarlierKeysDecision.Land
        assertTrue((decision.verdict as FfiBindingVerdict.Mint).key.contentEquals(cur))
    }

    @Test
    fun `the held conversation key is minted under`() = runTest {
        val ports = FakePorts(predecessors = mutableListOf(k0, k1), link = FfiOwnLink.Key(k0))
        val decision = runEarlierKeysGate(ports) as EarlierKeysDecision.Land
        assertTrue((decision.verdict as FfiBindingVerdict.Mint).key.contentEquals(k0))
    }

    @Test
    fun `the route's own read of the link is used and nothing is fetched again`() = runTest {
        val ports = FakePorts(link = FfiOwnLink.Absent)
        assertEquals(EarlierKeysDecision.Park, runEarlierKeysGate(ports, prefetched = FfiOwnLink.Key(k0)))
        assertTrue(ports.reads.isEmpty())
    }

    @Test
    fun `a device bound under the current key with no conversations parks once the record names an earlier key`() =
        runTest {
            // Landed before it learned the link (contract 2.1): the next
            // landing starts from the record and parks.
            val ports = FakePorts(bound = cur, recorded = FfiOwnLink.Key(k0), link = FfiOwnLink.Key(k0))
            assertEquals(EarlierKeysDecision.Park, runEarlierKeysGate(ports))
            assertTrue(ports.verdictLinks.first() is FfiOwnLink.Key)
        }

    @Test
    fun `a device bound under the current key that holds conversations keeps its binding`() = runTest {
        val ports = FakePorts(bound = cur, holds = true, recorded = FfiOwnLink.Key(k0))
        val decision = runEarlierKeysGate(ports) as EarlierKeysDecision.Land
        assertEquals(FfiBindingVerdict.KeepUnderCurrentKey, decision.verdict)
        assertTrue(ports.reads.isEmpty())
    }

    @Test
    fun `a verdict that cannot be taken parks`() = runTest {
        val ports = object : EarlierKeysGatePorts by FakePorts() {
            override fun verdict(
                boundKey: ByteArray?,
                currentKey: ByteArray,
                predecessorKeys: List<ByteArray>,
                link: FfiOwnLink,
                holdsConversations: Boolean,
            ): FfiBindingVerdict = throw IllegalArgumentException("bad-key-length")
        }
        assertEquals(EarlierKeysDecision.Park, runEarlierKeysGate(ports))
    }

    // ---- the parked retry -------------------------------------------------

    @Test
    fun `the parked retry lands once the backup supplies the earlier key`() = runTest {
        val ports = FakePorts(link = FfiOwnLink.Key(k0))
        assertEquals(EarlierKeysDecision.Park, runEarlierKeysGate(ports))
        // A look without the key: still parked, the failed look is not shown.
        assertFalse(lookForEarlierKeys(fetchBackupPredecessors = { throw IllegalStateException("offline") }, ports))
        // The backup now carries k0.
        assertTrue(lookForEarlierKeys(fetchBackupPredecessors = { ports.predecessors += k0 }, ports))
    }

    @Test
    fun `the parked retry lands when the link went away`() = runTest {
        val ports = FakePorts(link = FfiOwnLink.Key(k0))
        assertEquals(EarlierKeysDecision.Park, runEarlierKeysGate(ports))
        ports.link = FfiOwnLink.Absent
        assertTrue(lookForEarlierKeys(fetchBackupPredecessors = {}, ports))
    }

    @Test
    fun `there is no time limit and no fallback to the current key`() = runTest {
        val ports = FakePorts(link = FfiOwnLink.Key(k0))
        repeat(50) { assertFalse(lookForEarlierKeys(fetchBackupPredecessors = {}, ports)) }
        assertEquals(5L * 60L * 1000L, EARLIER_KEYS_RETRY_MS)
    }

    // ---- one landing at a time while parked -------------------------------

    @Test
    fun `two concurrent landings of a parked account run once`() = runTest {
        val guard = ParkedLandingGuard()
        var parked = true
        var landings = 0
        val land: suspend () -> String = {
            // The landing suspends half way (a network call), then lands.
            kotlinx.coroutines.delay(100)
            landings += 1
            parked = false
            "landed"
        }
        val look = async { guard.runIfParked({ parked }, land) }
        val pairing = async { guard.runIfParked({ parked }, land) }
        val answers = listOf(look.await(), pairing.await())
        assertEquals(1, landings)
        assertEquals(listOf("landed", null), answers)
    }

    @Test
    fun `a landing that did not land leaves the next one free to run`() = runTest {
        val guard = ParkedLandingGuard()
        var runs = 0
        repeat(3) { guard.runIfParked({ true }) { runs += 1 } }
        assertEquals(3, runs)
    }

    // ---- the link answer --------------------------------------------------

    private val noCall: (ByteArray?, ByteArray) -> ByteArray? = { _, _ -> error("not called") }

    @Test
    fun `no link, an empty one, or undecodable base64 reads Absent`() {
        assertEquals(FfiOwnLink.Absent, ownLinkOf(null, cur, noCall))
        assertEquals(FfiOwnLink.Absent, ownLinkOf("", cur, noCall))
        assertEquals(FfiOwnLink.Absent, ownLinkOf("%%%not-base64", cur, noCall))
    }

    @Test
    fun `a link that does not verify reads Absent`() {
        assertEquals(FfiOwnLink.Absent, ownLinkOf(base64Encode(ByteArray(201)), cur) { _, _ -> null })
        assertEquals(
            FfiOwnLink.Absent,
            ownLinkOf(base64Encode(ByteArray(201)), cur) { _, _ -> throw IllegalArgumentException("bad-key-length") },
        )
    }

    @Test
    fun `a link naming the current key itself reads Absent`() {
        assertEquals(FfiOwnLink.Absent, ownLinkOf(base64Encode(ByteArray(201)), cur) { _, _ -> cur.copyOf() })
    }

    @Test
    fun `a verified link names the conversation key, checked against this device's own key`() {
        var trusted: ByteArray? = null
        val answer = ownLinkOf(base64Encode(ByteArray(201) { 3 }), cur) { link, current ->
            assertEquals(201, link?.size)
            trusted = current
            k0.copyOf()
        }
        assertArrayEquals(k0, (answer as FfiOwnLink.Key).key)
        assertArrayEquals(cur, trusted)
    }

    // ---- the local record -------------------------------------------------

    @Test
    fun `the record round-trips a key and none, and refuses other bytes`() {
        val withKey = OwnConversationKeyCodec.decode(OwnConversationKeyCodec.encode(OwnConversationKeyRecord(k0, 42L)))!!
        assertArrayEquals(k0, withKey.conversationKey)
        assertEquals(42L, withKey.readAtMs)
        val none = OwnConversationKeyCodec.decode(OwnConversationKeyCodec.encode(OwnConversationKeyRecord(null, 7L)))!!
        assertNull(none.conversationKey)
        assertEquals(7L, none.readAtMs)
        assertNull(OwnConversationKeyCodec.decode(ByteArray(0)))
        assertNull(OwnConversationKeyCodec.decode(byteArrayOf(2, 0, 0, 0, 0, 0, 0, 0, 0, 0)))
        assertNull(OwnConversationKeyCodec.decode(byteArrayOf(1, 1, 0)))
    }

    @Test
    fun `Unknown leaves the record as it was`() {
        assertNull(recordOfLink(FfiOwnLink.Unknown, 1L))
        assertNull(recordOfLink(FfiOwnLink.Absent, 1L)!!.conversationKey)
        assertArrayEquals(k0, recordOfLink(FfiOwnLink.Key(k0), 1L)!!.conversationKey)
        assertEquals(FfiOwnLink.Absent, linkOfRecord(null))
        assertEquals(FfiOwnLink.Absent, linkOfRecord(OwnConversationKeyRecord(null, 1L)))
        assertArrayEquals(k0, (linkOfRecord(OwnConversationKeyRecord(k0, 1L)) as FfiOwnLink.Key).key)
    }

    // ---- the DeviceGroup lineage guard -----------------------------------

    @Test
    fun `with a record naming an earlier key, a seed that does not derive it names no DeviceGroup`() {
        assertTrue(lineageFollowsLink(null, cur))
        assertTrue(lineageFollowsLink(k0, k0.copyOf()))
        assertFalse(lineageFollowsLink(k0, cur))
    }

    // ---- the own predecessor store, read for a recovery row --------------

    private class MapBlobs : SealedBlobStorage {
        val map = mutableMapOf<String, ByteArray>()
        val unreadable = mutableSetOf<String>()
        override fun read(name: String): ByteArray? = if (name in unreadable) null else map[name]?.copyOf()
        override fun write(name: String, plaintext: ByteArray) {
            map[name] = plaintext.copyOf()
        }
        override fun delete(name: String) {
            map.remove(name)
        }
        override fun exists(name: String): Boolean = name in map || name in unreadable
    }

    @Test
    fun `an unreadable predecessor store is not none`() {
        val blobs = MapBlobs()
        val store = OwnPredecessorStore(blobs)
        val account = base64Encode(ByteArray(16) { 7 })
        assertEquals(0, store.loadOrNull(account)!!.size)
        blobs.unreadable += KeyRotationFiles.hexOf(account)!! + KeyRotationFiles.PREDECESSOR_SUFFIX
        assertNull(store.loadOrNull(account))
        assertEquals(0, store.load(account).size)
    }
}
