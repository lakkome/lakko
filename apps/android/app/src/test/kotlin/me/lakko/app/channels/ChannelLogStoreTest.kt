// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for the persisted channel log — the fix for the
 * Channels tab emptying on every relaunch. Run as `:app:testDebugUnitTest`,
 * no emulator, no UniFFI native library ([ChannelsUiState] is built with
 * `publisher = null`, so `bind` exercises only the store half).
 *
 *   1. The codec the prefs store and the in-memory store share: channel
 *      rows (with the handle), the `channel -> epoch -> seed` map, the
 *      rotation state; corrupt input reads as absent, a bad row is skipped.
 *   2. The store contract over [InMemoryChannelLogStore], incl. delete.
 *   3. [channelsToReopen] — which channels a relaunch re-opens publisher
 *      keys for.
 *   4. [ChannelsUiState] write-through + load on bind, unbind keeping the
 *      persisted state, forgetAccount deleting it.
 */
class ChannelLogStoreTest {

    private val account = "YWNjb3VudC1pZC0xNi1ieXRlcw=="
    private val otherAccount = "b3RoZXItYWNjb3VudC0xNi1ieXRl"

    private fun channel(idByte: Int, name: String = "Channel $idByte", handle: String = "") =
        ChannelHandle(
            id = "%02x".format(idByte).repeat(16),
            localName = name,
            localDescription = "about $idByte",
            addedAtMs = 1_758_000_000_000uL + idByte.toULong(),
            handle = handle,
        )

    private fun seed(fill: Int): ByteArray = ByteArray(32) { fill.toByte() }

    // ------------------------------------------------------------------
    // 1. Codec
    // ------------------------------------------------------------------

    @Test
    fun `channel rows round-trip with the handle and a 64-bit timestamp`() {
        val rows = listOf(
            channel(1, handle = "news"),
            channel(2),
            ChannelHandle(
                id = "ff".repeat(16),
                localName = "",
                localDescription = "",
                addedAtMs = ULong.MAX_VALUE,
                handle = "max",
            ),
        )
        val decoded = decodeChannels(encodeChannels(rows))
        assertEquals(rows, decoded)
    }

    @Test
    fun `channel codec treats garbage as absent and skips a row without an id`() {
        assertNull(decodeChannels("not json"))
        assertNull(decodeChannels("{\"id\":\"x\"}"))
        val raw = """[{"id":"","localName":"a","localDescription":"","addedAtMs":"1"},
            |{"id":"aa","localName":"b","localDescription":"","addedAtMs":"nope"},
            |{"id":"bb","localName":"c","localDescription":"d","addedAtMs":"7"}]""".trimMargin()
        val decoded = decodeChannels(raw)
        assertEquals(1, decoded?.size)
        assertEquals("bb", decoded?.single()?.id)
        assertEquals(7uL, decoded?.single()?.addedAtMs)
        // A row written before the handle field existed reads with an empty handle.
        assertEquals("", decoded?.single()?.handle)
    }

    @Test
    fun `subscribed seeds round-trip per channel and epoch`() {
        val a = channel(1).id
        val b = channel(2).id
        val seeds = mapOf(
            a to mapOf(0u to seed(0x10), 3u to seed(0x13), UInt.MAX_VALUE to seed(0x1f)),
            b to mapOf(7u to seed(0x27)),
        )
        val decoded = decodeSubscribedSeeds(encodeSubscribedSeeds(seeds))
        requireNotNull(decoded)
        assertEquals(setOf(a, b), decoded.keys)
        assertEquals(setOf(0u, 3u, UInt.MAX_VALUE), decoded.getValue(a).keys)
        assertArrayEquals(seed(0x13), decoded.getValue(a).getValue(3u))
        assertArrayEquals(seed(0x1f), decoded.getValue(a).getValue(UInt.MAX_VALUE))
        assertArrayEquals(seed(0x27), decoded.getValue(b).getValue(7u))
    }

    @Test
    fun `subscribed seeds codec skips a wrong-length seed and an out-of-range epoch`() {
        assertNull(decodeSubscribedSeeds("[1,2"))
        val short = java.util.Base64.getEncoder().encodeToString(ByteArray(16))
        val good = java.util.Base64.getEncoder().encodeToString(seed(0x42))
        val raw = """[{"channelId":"aa","seeds":[
            |{"epoch":1,"seedB64":"$short"},
            |{"epoch":-1,"seedB64":"$good"},
            |{"epoch":4294967296,"seedB64":"$good"},
            |{"epoch":2,"seedB64":"%%%"},
            |{"epoch":5,"seedB64":"$good"}]},
            |{"channelId":"bb","seeds":[{"epoch":1,"seedB64":"$short"}]}]""".trimMargin()
        val decoded = decodeSubscribedSeeds(raw)
        requireNotNull(decoded)
        // Channel bb had only an unusable seed, so it is absent rather than empty.
        assertEquals(setOf("aa"), decoded.keys)
        assertEquals(setOf(5u), decoded.getValue("aa").keys)
        assertArrayEquals(seed(0x42), decoded.getValue("aa").getValue(5u))
    }

    @Test
    fun `rotation state round-trips including the unsigned extremes`() {
        val states = listOf(
            StoredRotationState(lastRotationMs = 0uL, postsSinceRotation = 0u),
            StoredRotationState(lastRotationMs = 1_758_000_000_123uL, postsSinceRotation = 999u),
            StoredRotationState(lastRotationMs = ULong.MAX_VALUE, postsSinceRotation = UInt.MAX_VALUE),
        )
        for (s in states) {
            assertEquals(s, decodeRotationState(encodeRotationState(s)))
        }
        assertNull(decodeRotationState(""))
        assertNull(decodeRotationState("""{"lastRotationMs":"-5","postsSinceRotation":1}"""))
        assertNull(decodeRotationState("""{"lastRotationMs":"5","postsSinceRotation":-1}"""))
    }

    // ------------------------------------------------------------------
    // 2. Store contract
    // ------------------------------------------------------------------

    @Test
    fun `in-memory store keeps accounts apart and delete drops everything for one`() {
        val store = InMemoryChannelLogStore()
        assertTrue(store.loadChannels(account).isEmpty())
        assertTrue(store.loadSubscribedSeeds(account).isEmpty())
        assertNull(store.loadRotationState(account, channel(1).id))

        store.saveChannels(account, listOf(channel(1), channel(2)))
        store.saveChannels(otherAccount, listOf(channel(3)))
        store.saveSubscribedSeeds(account, mapOf(channel(1).id to mapOf(0u to seed(1))))
        store.saveRotationState(account, channel(1).id, StoredRotationState(10uL, 2u))
        store.saveRotationState(account, channel(2).id, StoredRotationState(20uL, 0u))
        store.saveRotationState(otherAccount, channel(3).id, StoredRotationState(30uL, 1u))

        assertEquals(listOf(channel(1), channel(2)), store.loadChannels(account))
        assertEquals(listOf(channel(3)), store.loadChannels(otherAccount))
        assertEquals(StoredRotationState(10uL, 2u), store.loadRotationState(account, channel(1).id))

        store.deleteRotationState(account, channel(2).id)
        assertNull(store.loadRotationState(account, channel(2).id))
        assertEquals(StoredRotationState(10uL, 2u), store.loadRotationState(account, channel(1).id))

        store.delete(account)
        assertTrue(store.loadChannels(account).isEmpty())
        assertTrue(store.loadSubscribedSeeds(account).isEmpty())
        assertNull(store.loadRotationState(account, channel(1).id))
        // The other account is untouched.
        assertEquals(listOf(channel(3)), store.loadChannels(otherAccount))
        assertEquals(StoredRotationState(30uL, 1u), store.loadRotationState(otherAccount, channel(3).id))
    }

    // ------------------------------------------------------------------
    // 3. Owner re-open selection
    // ------------------------------------------------------------------

    @Test
    fun `channelsToReopen picks listed channels with a signing seed and no live keys, in list order`() {
        val created = channel(1)
        val coPublished = channel(2)
        val subscribed = channel(3)
        val alreadyOpen = channel(4)
        val removed = channel(5)
        val list = listOf(coPublished, subscribed, alreadyOpen, created)
        val seeds = mapOf(
            created.id to ByteArray(32) { 1 },
            coPublished.id to ByteArray(32) { 2 },
            alreadyOpen.id to ByteArray(32) { 4 },
            removed.id to ByteArray(32) { 5 },
        )
        val selected = channelsToReopen(
            channels = list,
            signingSeedByChannel = seeds,
            channelsWithKeys = setOf(alreadyOpen.id),
        )
        assertEquals(listOf(coPublished, created), selected)
    }

    @Test
    fun `channelsToReopen is empty without signing seeds or without channels`() {
        assertTrue(channelsToReopen(listOf(channel(1)), emptyMap(), emptySet()).isEmpty())
        assertTrue(channelsToReopen(emptyList(), mapOf(channel(1).id to ByteArray(32)), emptySet()).isEmpty())
    }

    // ------------------------------------------------------------------
    // 4. ChannelsUiState over the store
    // ------------------------------------------------------------------

    @Test
    fun `bind loads the persisted list and seeds and mutations write through`() = runBlocking {
        val store = InMemoryChannelLogStore()
        val first = ChannelsUiState(logStore = store)
        first.bind(account, ByteArray(64))
        assertEquals(account, first.boundAccount)

        first.addChannel(channel(1, handle = "news"))
        first.addChannel(channel(2))
        first.setSubscribedSeed(channel(2).id, 0u, seed(0x20))
        first.setSubscribedSeed(channel(2).id, 1u, seed(0x21))
        first.updateLocalMeta(channel(1).id, localName = "Renamed")
        first.saveRotationState(channel(1).id, StoredRotationState(5uL, 1u))

        // A second state bound to the same store — what a relaunch sees.
        val relaunched = ChannelsUiState(logStore = store)
        relaunched.bind(account, ByteArray(64))
        val list = relaunched.channels.value
        assertEquals(listOf(channel(2).id, channel(1).id), list.map { it.id })
        assertEquals("Renamed", list.last().localName)
        assertEquals("news", list.last().handle)
        val seeds = relaunched.subscribedSeeds(channel(2).id)
        assertEquals(setOf(0u, 1u), seeds.keys)
        assertArrayEquals(seed(0x21), seeds.getValue(1u))
        assertEquals(StoredRotationState(5uL, 1u), relaunched.rotationState(channel(1).id))

        // Removing a channel drops its row, its seeds and its rotation state.
        relaunched.removeChannel(channel(2).id)
        relaunched.removeChannel(channel(1).id)
        assertTrue(store.loadChannels(account).isEmpty())
        assertTrue(store.loadSubscribedSeeds(account).isEmpty())
        assertNull(store.loadRotationState(account, channel(1).id))
    }

    @Test
    fun `clearSubscriberKeys drops the persisted seeds for that channel only`() = runBlocking {
        val store = InMemoryChannelLogStore()
        val state = ChannelsUiState(logStore = store)
        state.bind(account, ByteArray(64))
        state.setSubscribedSeed(channel(1).id, 0u, seed(1))
        state.setSubscribedSeed(channel(2).id, 0u, seed(2))
        state.clearSubscriberKeys(channel(1).id)
        assertEquals(setOf(channel(2).id), store.loadSubscribedSeeds(account).keys)
    }

    @Test
    fun `unbind keeps the persisted log and forgetAccount deletes it`() = runBlocking {
        val store = InMemoryChannelLogStore()
        val state = ChannelsUiState(logStore = store)
        state.bind(account, ByteArray(64))
        state.addChannel(channel(1))
        state.setSubscribedSeed(channel(1).id, 0u, seed(1))
        state.saveRotationState(channel(1).id, StoredRotationState(1uL, 0u))

        state.unbind()
        assertNull(state.boundAccount)
        assertTrue(state.channels.value.isEmpty())
        assertTrue(state.subscribedSeeds(channel(1).id).isEmpty())
        assertEquals(1, store.loadChannels(account).size)

        state.bind(account, ByteArray(64))
        assertEquals(1, state.channels.value.size)
        assertEquals(1, state.subscribedSeeds(channel(1).id).size)

        state.forgetAccount()
        assertNull(state.boundAccount)
        assertTrue(store.loadChannels(account).isEmpty())
        assertTrue(store.loadSubscribedSeeds(account).isEmpty())
        assertNull(store.loadRotationState(account, channel(1).id))
    }

    @Test
    fun `binding another account swaps the in-memory list without mixing the stores`() = runBlocking {
        val store = InMemoryChannelLogStore()
        val state = ChannelsUiState(logStore = store)
        state.bind(account, ByteArray(64))
        state.addChannel(channel(1))
        state.bind(otherAccount, ByteArray(64))
        assertTrue(state.channels.value.isEmpty())
        state.addChannel(channel(2))
        assertEquals(listOf(channel(1)), store.loadChannels(account))
        assertEquals(listOf(channel(2)), store.loadChannels(otherAccount))
        // Re-binding the same account is a no-op (the store is not re-read).
        state.bind(otherAccount, ByteArray(64))
        assertEquals(listOf(channel(2)), state.channels.value)
    }

    @Test
    fun `a mutation before bind stays in memory and is replaced by the persisted list on bind`() = runBlocking {
        val store = InMemoryChannelLogStore()
        store.saveChannels(account, listOf(channel(9)))
        val state = ChannelsUiState(logStore = store)
        state.addChannel(channel(1))
        assertTrue(store.loadChannels(otherAccount).isEmpty())
        assertEquals(listOf(channel(9)), store.loadChannels(account))
        state.bind(account, ByteArray(64))
        assertEquals(listOf(channel(9)), state.channels.value)
    }

    @Test
    fun `rotation state is unavailable before bind`() {
        val state = ChannelsUiState(logStore = InMemoryChannelLogStore())
        assertNull(state.rotationState(channel(1).id))
        // A save before bind is a no-op rather than a crash.
        state.saveRotationState(channel(1).id, StoredRotationState(1uL, 1u))
        assertNull(state.rotationState(channel(1).id))
    }
}
