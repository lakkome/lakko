// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import uniffi.lakko_ffi_uniffi.InfoSection

/**
 * docs/ux/second-device.md section 4a — the account's channels on its other
 * devices: the channel subscription register written and applied
 * ([ChannelSubscriptionSync]), channels subscribed before this build written
 * once, the reader device of an owner or a publisher, and the trap rule (no
 * device founds a PublisherGroup outside New channel). Robolectric for a real
 * `SharedPreferences` + `org.json` under [ChannelsUiState]; the register and
 * the native key derivation are faked.
 */
@RunWith(RobolectricTestRunner::class)
class ChannelSecondDeviceTest {

    private val account = "CgoKCgoKCgoKCgoKCgoKCg=="
    private val chan = "12".repeat(16)
    private val other = "34".repeat(16)
    private val secret = ByteArray(32) { 7 }
    private val readKey = ByteArray(32) { 8 }
    private val groupId = ByteArray(32) { 9 }

    private fun register(
        id: String = chan,
        hlc: ULong = 1_000_000uL,
        subscribed: Boolean = true,
        role: ChannelAccountRole = ChannelAccountRole.SUBSCRIBER,
        secret: ByteArray = this.secret,
        readKey: ByteArray = ByteArray(0),
        groupId: ByteArray = ByteArray(0),
        name: String = "News",
        handle: String = "news",
    ) = ChannelRegisterView(id, hlc, subscribed, role, secret, readKey, groupId, name, handle)

    /** The register as a fake fold: a write lands only on the planned stamp. */
    private class FakePort(var held: List<ChannelRegisterView>, override val boundAccount: String?) : ChannelRegisterPort {
        val writes = ArrayList<ChannelRegisterView>()
        val leaves = ArrayList<String>()
        override fun registers(): List<ChannelRegisterView> = held

        override fun write(value: ChannelRegisterView): Boolean {
            val current = held.firstOrNull { it.channelIdHex == value.channelIdHex }?.hlc ?: 0uL
            if (current != value.hlc) return false
            writes += value
            held = held.filter { it.channelIdHex != value.channelIdHex } +
                ChannelRegisterView(
                    value.channelIdHex, current + 1u, true, value.role, value.subscriberSecret,
                    value.readKey, value.publisherGroupId, value.name, value.handle, value.ownerAccountKey,
                )
            return true
        }

        override fun leave(channelIdHex: String): Boolean {
            leaves += channelIdHex
            return true
        }
    }

    private class FakeStores : ChannelReconcileStores {
        val readKeys = HashMap<String, ByteArray>()
        val retired = ArrayList<String>()
        val dropped = ArrayList<String>()
        var fresh = 0
        override fun readKey(account: String, channelIdHex: String): ByteArray? = readKeys[channelIdHex]
        override fun putReadKey(account: String, channelIdHex: String, readKey: ByteArray) {
            readKeys[channelIdHex] = readKey
        }

        // Stands in for the core's Ed25519 public key of the seed.
        override fun publicKeyOf(subscriberSecret: ByteArray): ByteArray = subscriberSecret.map { (it + 100).toByte() }.toByteArray()
        override fun freshSubscriberKeys(): ChannelSubscriberKeys {
            fresh += 1
            return ChannelSubscriberKeys(ByteArray(32) { 50 }, ByteArray(32) { 51 })
        }

        override fun retireOldKey(channelIdHex: String, old: ChannelSubscriberKeys) {
            retired += channelIdHex
        }

        override fun dropLeft(state: ChannelsUiState, channelIdHex: String) {
            dropped += channelIdHex
            state.clearSubscriberKeys(channelIdHex)
            state.removeChannel(channelIdHex)
        }

        val registered = HashSet<String>()
        val registerCalls = ArrayList<String>()
        override fun accountKeyRegistered(account: String, channelIdHex: String, publicKeyHex: String): Boolean =
            "$channelIdHex|$publicKeyHex" in registered

        override suspend fun registerAccountKey(state: ChannelsUiState, channelIdHex: String, keys: ChannelSubscriberKeys): Boolean {
            registerCalls += channelIdHex
            registered += "$channelIdHex|${ChannelsComposer.bytesToHex(keys.publicKey)}"
            return true
        }
    }

    private fun orphanStore(): OrphanedChannelsStore =
        OrphanedChannelsStore(RuntimeEnvironment.getApplication().getSharedPreferences("second-device-orphans", Context.MODE_PRIVATE))

    private fun boundState(orphans: OrphanedChannelsStore? = null): ChannelsUiState = runBlocking {
        val context = RuntimeEnvironment.getApplication()
        val s = ChannelsUiState(
            prefs = context.getSharedPreferences("second-device-subscriber", Context.MODE_PRIVATE),
            publisherPrefs = context.getSharedPreferences("second-device-publisher", Context.MODE_PRIVATE),
            logStore = InMemoryChannelLogStore(),
            orphanStore = orphans,
            coPublished = InMemoryCoPublishedJournalMarks(),
        )
        s.bind(account, ByteArray(64))
        s
    }

    private fun handle(id: String = chan, addedAtMs: ULong = 1_000uL, role: ChannelAccountRole = ChannelAccountRole.SUBSCRIBER) =
        ChannelHandle(id = id, localName = "News", localDescription = "", addedAtMs = addedAtMs, handle = "news", role = role)

    // ---- apply ---------------------------------------------------------

    @Test
    fun `a channel the account owns elsewhere arrives with the shared key and the read key`() {
        val state = boundState()
        val port = FakePort(listOf(register(role = ChannelAccountRole.OWNER, readKey = readKey)), account)
        val stores = FakeStores()
        ChannelSubscriptionSync.reconcileLocked(state, port, stores)

        val listed = state.channels.value.single()
        assertEquals(chan, listed.id)
        assertEquals(ChannelAccountRole.OWNER, listed.role)
        assertEquals("news", listed.handle)
        // The register's key, never a fresh one (one key per account and channel).
        assertArrayEquals(secret, state.subscriberKeysByChannel.value[chan]!!.secret)
        assertEquals(0, stores.fresh)
        assertArrayEquals(readKey, stores.readKeys[chan])
        // This device holds no PublisherGroup: it reads the channel.
        assertEquals(ChannelAccountRole.OWNER, state.readerDeviceRoleOf(chan))
        assertTrue(port.writes.isEmpty())
    }

    @Test
    fun `a co-publisher's channel arrives with the group it joined, read here`() {
        val state = boundState()
        val port = FakePort(listOf(register(role = ChannelAccountRole.CO_PUBLISHER, groupId = groupId)), account)
        ChannelSubscriptionSync.reconcileLocked(state, port, FakeStores())
        assertArrayEquals(groupId, state.joinedGroupIdOf(chan))
        assertEquals(ChannelAccountRole.CO_PUBLISHER, state.accountRoleOf(chan))
        assertEquals(ChannelAccountRole.CO_PUBLISHER, state.readerDeviceRoleOf(chan))
    }

    @Test
    fun `a leave written on another device drops the channel without the service`() {
        val state = boundState()
        state.addChannel(handle(addedAtMs = 1_000uL))
        state.setSubscriberKeys(chan, ChannelSubscriberKeys(secret, ByteArray(32) { 1 }))
        // Written at 2 000 ms, after this device listed it at 1 000 ms.
        val port = FakePort(listOf(register(hlc = 2_000_000uL, subscribed = false, secret = ByteArray(0))), account)
        val stores = FakeStores()
        ChannelSubscriptionSync.reconcileLocked(state, port, stores)
        assertEquals(listOf(chan), stores.dropped)
        assertTrue(state.channels.value.isEmpty())
        assertTrue(port.writes.isEmpty())
    }

    @Test
    fun `a subscription made here after an older leave is written again`() {
        val state = boundState()
        state.addChannel(handle(addedAtMs = 5_000uL))
        state.setSubscriberKeys(chan, ChannelSubscriberKeys(secret, ByteArray(32) { 1 }))
        val port = FakePort(listOf(register(hlc = 2_000_000uL, subscribed = false, secret = ByteArray(0))), account)
        val stores = FakeStores()
        ChannelSubscriptionSync.reconcileLocked(state, port, stores)
        assertTrue(stores.dropped.isEmpty())
        val written = port.writes.single()
        assertTrue(written.subscribed)
        assertArrayEquals(secret, written.subscriberSecret)
        assertEquals(2_000_000uL, written.hlc)
    }

    @Test
    fun `a different local key adopts the register's key and retires the old one`() {
        val state = boundState()
        state.addChannel(handle())
        state.setSubscriberKeys(chan, ChannelSubscriberKeys(ByteArray(32) { 3 }, ByteArray(32) { 4 }))
        val port = FakePort(listOf(register()), account)
        val stores = FakeStores()
        ChannelSubscriptionSync.reconcileLocked(state, port, stores)
        assertArrayEquals(secret, state.subscriberKeysByChannel.value[chan]!!.secret)
        assertEquals(listOf(chan), stores.retired)
        assertTrue(port.writes.isEmpty())
    }

    @Test
    fun `left seeds of a channel never listed are forgotten`() {
        val state = boundState()
        state.addCarriedSeeds(mapOf(other to mapOf(4u to ByteArray(32) { 2 })))
        val port = FakePort(listOf(register(id = other, subscribed = false, secret = ByteArray(0))), account)
        ChannelSubscriptionSync.reconcileLocked(state, port, FakeStores())
        assertTrue(state.subscribedSeeds(other).isEmpty())
    }

    @Test
    fun `nothing runs while the register belongs to another account`() {
        val state = boundState()
        val port = FakePort(listOf(register()), "CwsLCwsLCwsLCwsLCwsLCw==")
        ChannelSubscriptionSync.reconcileLocked(state, port, FakeStores())
        assertTrue(state.channels.value.isEmpty())
    }

    // ---- write and the one-time migration ---------------------------------

    @Test
    fun `a channel subscribed before this build is written to the register once`() {
        val state = boundState()
        state.addChannel(handle())
        state.setSubscriberKeys(chan, ChannelSubscriberKeys(secret, ByteArray(32) { 1 }))
        val port = FakePort(emptyList(), account)
        val stores = FakeStores()
        ChannelSubscriptionSync.reconcileLocked(state, port, stores)
        val written = port.writes.single()
        assertEquals(ChannelAccountRole.SUBSCRIBER, written.role)
        assertArrayEquals(secret, written.subscriberSecret)
        assertEquals("News", written.name)
        assertEquals("news", written.handle)
        assertEquals(0uL, written.hlc)
        // The second pass finds the identical value: nothing more is written.
        ChannelSubscriptionSync.reconcileLocked(state, port, stores)
        assertEquals(1, port.writes.size)
    }

    @Test
    fun `a channel this device publishes is written as the owner's, with a key minted for it`() {
        val state = boundState()
        state.addChannel(handle())
        state.setKeys(chan, ChannelPublisherKeys(seed = ByteArray(32) { 5 }, signingKeySeed = ByteArray(32) { 6 }))
        val port = FakePort(emptyList(), account)
        val stores = FakeStores()
        ChannelSubscriptionSync.reconcileLocked(state, port, stores)
        assertEquals(1, stores.fresh)
        val written = port.writes.single()
        assertEquals(ChannelAccountRole.OWNER, written.role)
        assertArrayEquals(ByteArray(32) { 50 }, written.subscriberSecret)
        // It publishes here: not a reader device.
        assertNull(state.readerDeviceRoleOf(chan))
    }

    @Test
    fun `a read key learned here fills the register`() {
        val state = boundState()
        state.addChannel(handle())
        state.setSubscriberKeys(chan, ChannelSubscriberKeys(secret, ByteArray(32) { 1 }))
        val port = FakePort(listOf(register(hlc = 9uL)), account)
        val stores = FakeStores().apply { readKeys[chan] = readKey }
        ChannelSubscriptionSync.reconcileLocked(state, port, stores)
        val written = port.writes.single()
        assertArrayEquals(readKey, written.readKey)
        assertEquals(9uL, written.hlc)
    }

    @Test
    fun `a reconcile planned against an older register never overwrites a leave`() {
        val local = LocalChannelFacts(chan, ChannelAccountRole.SUBSCRIBER, false, secret, null, null, "News", "news", 1_000uL)
        val plan = planChannelReconcile(listOf(local), listOf(register(hlc = 5uL)), emptySet())
        assertTrue(plan.write.isEmpty())
        // A write planned at stamp 5 meets a leave written meanwhile (stamp 6).
        val port = FakePort(listOf(register(hlc = 6uL, subscribed = false, secret = ByteArray(0))), account)
        assertFalse(port.write(register(hlc = 5uL, name = "Other")))
    }

    // ---- the plan --------------------------------------------------------

    @Test
    fun `a publishing device never drops its channel on a leave`() {
        val local = LocalChannelFacts(chan, ChannelAccountRole.OWNER, true, secret, null, null, "News", "news", 1_000uL)
        val plan = planChannelReconcile(listOf(local), listOf(register(hlc = 9_000_000uL, subscribed = false, secret = ByteArray(0))), emptySet())
        assertTrue(plan.drop.isEmpty())
    }

    @Test
    fun `the register value merges what this device knows`() {
        val held = register(role = ChannelAccountRole.OWNER, readKey = readKey, name = "Old", handle = "old_name")
        // A reader device: the role stays the register's, its read key too.
        val reader = LocalChannelFacts(chan, ChannelAccountRole.OWNER, false, secret, null, null, "", "", 1uL)
        val kept = desiredChannelRegister(reader, held)!!
        assertTrue(kept.sameValueAs(held))
        // A reader with another name keeps the register's (no two readers
        // overwrite each other); the publishing device's name replaces it.
        val readerName = desiredChannelRegister(
            LocalChannelFacts(chan, ChannelAccountRole.OWNER, false, secret, null, null, "  New  ", "new_name", 1uL),
            held,
        )!!
        assertEquals("Old", readerName.name)
        assertEquals("old_name", readerName.handle)
        val renamed = desiredChannelRegister(
            LocalChannelFacts(chan, ChannelAccountRole.OWNER, true, secret, null, null, "  New  ", "new_name", 1uL),
            held,
        )!!
        assertEquals("New", renamed.name)
        assertEquals("new_name", renamed.handle)
        // An empty register name is filled by a reader.
        assertEquals(
            "New",
            desiredChannelRegister(
                LocalChannelFacts(chan, ChannelAccountRole.OWNER, false, secret, null, null, "New", "", 1uL),
                register(role = ChannelAccountRole.OWNER, name = "", handle = ""),
            )!!.name,
        )
        assertEquals("", registerName("a\nb"))
        assertEquals("", registerHandle("No"))
        // A co-publisher with no group id anywhere writes nothing.
        assertNull(
            desiredChannelRegister(
                LocalChannelFacts(chan, ChannelAccountRole.CO_PUBLISHER, true, secret, null, null, "News", "", 1uL),
                null,
            ),
        )
        // No key at all: nothing to write.
        assertNull(desiredChannelRegister(LocalChannelFacts(chan, ChannelAccountRole.SUBSCRIBER, false, null, null, null, "News", "", 1uL), null))
    }

    @Test
    fun `a co-publisher register carries the recorded owner key, and none without one`() {
        // ADR-0090 section 2.3 — the joining device's verified issuer key
        // goes into the register with the group id; with no owner key
        // anywhere nothing is written; an owner's register carries none.
        val group = ByteArray(32) { 3 }
        val owner = ByteArray(32) { 9 }
        val joined = LocalChannelFacts(
            chan, ChannelAccountRole.CO_PUBLISHER, true, secret, null, group, "News", "", 1uL,
            ownerAccountKey = owner,
        )
        assertArrayEquals(owner, desiredChannelRegister(joined, null)!!.ownerAccountKey)
        val noKey = LocalChannelFacts(chan, ChannelAccountRole.CO_PUBLISHER, true, secret, null, group, "News", "", 1uL)
        assertNull(desiredChannelRegister(noKey, null))
        // Another device of the account keeps the key the register holds.
        val held = register(role = ChannelAccountRole.CO_PUBLISHER, groupId = group).let {
            ChannelRegisterView(it.channelIdHex, it.hlc, true, it.role, it.subscriberSecret, it.readKey, group, it.name, it.handle, owner)
        }
        assertArrayEquals(owner, desiredChannelRegister(noKey, held)!!.ownerAccountKey)
        val ownerSide = LocalChannelFacts(chan, ChannelAccountRole.OWNER, true, secret, null, null, "News", "", 1uL, ownerAccountKey = owner)
        assertEquals(0, desiredChannelRegister(ownerSide, null)!!.ownerAccountKey.size)
    }

    @Test
    fun `secrets never print`() {
        assertFalse(register(readKey = readKey).toString().contains("7"))
    }

    // ---- the reader device's screens ---------------------------------------

    @Test
    fun `the reader line names where the person posts`() {
        assertEquals("channels.reader_device.owner", readerDeviceLineKey(ChannelAccountRole.OWNER))
        assertEquals("channels.reader_device.publisher", readerDeviceLineKey(ChannelAccountRole.CO_PUBLISHER))
        assertNull(readerDeviceLineKey(ChannelAccountRole.SUBSCRIBER))
        assertEquals(ChannelAccountRole.OWNER, readerDeviceRole(ChannelAccountRole.OWNER, publishesHere = false, orphaned = false, keysReady = true))
        assertNull(readerDeviceRole(ChannelAccountRole.OWNER, publishesHere = true, orphaned = false, keysReady = true))
        assertNull(readerDeviceRole(ChannelAccountRole.OWNER, publishesHere = false, orphaned = false, keysReady = false))
        assertNull(readerDeviceRole(ChannelAccountRole.OWNER, publishesHere = false, orphaned = true, keysReady = true))
        assertNull(readerDeviceRole(ChannelAccountRole.SUBSCRIBER, publishesHere = false, orphaned = false, keysReady = true))
    }

    @Test
    fun `a reader device's info has a subscriber's blocks and no exit`() {
        val subscriberView = listOf(
            InfoSection.HANDLE,
            InfoSection.CHANNEL_DESCRIPTION,
            InfoSection.SUBSCRIBERS,
            InfoSection.MEMBERS_CAN_FIND_ME,
            InfoSection.UNSUBSCRIBE,
        )
        assertEquals(
            listOf(InfoSection.HANDLE, InfoSection.CHANNEL_DESCRIPTION, InfoSection.SUBSCRIBERS, InfoSection.MEMBERS_CAN_FIND_ME),
            readerDeviceSections(ChannelAccountRole.OWNER, subscriberView),
        )
        assertEquals(subscriberView, readerDeviceSections(null, subscriberView))
    }

    @Test
    fun `a reader device of the owner or a publisher announces no post`() {
        val held = listOf(
            handle(id = chan, role = ChannelAccountRole.OWNER),
            handle(id = other, role = ChannelAccountRole.SUBSCRIBER),
            handle(id = "56".repeat(16), role = ChannelAccountRole.CO_PUBLISHER),
        )
        assertEquals(
            setOf(chan, "56".repeat(16), "ab".repeat(16)),
            me.lakko.app.feed.channelsOwnedByAccount(held, setOf("AB".repeat(16))),
        )
    }

    @Test
    fun `the role survives the channel log`() {
        val store = InMemoryChannelLogStore()
        store.saveChannels(account, listOf(handle(role = ChannelAccountRole.CO_PUBLISHER), handle(id = other)))
        val back = store.loadChannels(account)
        assertEquals(ChannelAccountRole.CO_PUBLISHER, back[0].role)
        assertEquals(ChannelAccountRole.SUBSCRIBER, back[1].role)
    }

    // ---- the trap rule ---------------------------------------------------

    @Test
    fun `no path but New channel founds a PublisherGroup`() {
        assertEquals(PublisherGroupOpenMode.LOAD, publisherGroupOpenMode(joined = false, allowFound = false))
        assertEquals(PublisherGroupOpenMode.LOAD_JOINED, publisherGroupOpenMode(joined = true, allowFound = false))
        // Even New channel never founds a co-published channel's group.
        assertEquals(PublisherGroupOpenMode.LOAD_JOINED, publisherGroupOpenMode(joined = true, allowFound = true))
        assertEquals(PublisherGroupOpenMode.LOAD_OR_FOUND, publisherGroupOpenMode(joined = false, allowFound = true))
    }

    @Test
    fun `carried seeds never overwrite a held seed and are kept for a channel not listed`() {
        val state = boundState()
        state.setSubscribedSeed(chan, 41u, ByteArray(32) { 9 })
        val added = state.addCarriedSeeds(
            mapOf(chan to mapOf(41u to ByteArray(32) { 1 }, 42u to ByteArray(32) { 2 }), other to mapOf(3u to ByteArray(32) { 3 })),
        )
        assertEquals(2, added)
        assertArrayEquals(ByteArray(32) { 9 }, state.subscribedSeeds(chan)[41u])
        assertArrayEquals(ByteArray(32) { 2 }, state.subscribedSeeds(chan)[42u])
        assertArrayEquals(ByteArray(32) { 3 }, state.subscribedSeeds(other)[3u])
    }

    // ---- follow-up: the account's reader key, orphans --------------------

    @Test
    fun `a private rotation reseals to the account's key even above the mark`() {
        val own = "aa".repeat(32)
        val rows = listOf(
            SubscriberRow(1, "01".repeat(32), null),
            SubscriberRow(2, "02".repeat(32), null),
            SubscriberRow(9, own, null),
            SubscriberRow(10, "03".repeat(32), null),
        )
        assertEquals(listOf("01".repeat(32), "02".repeat(32)), ChannelReseal.resealKeys(rows, 2L))
        assertEquals(
            listOf("01".repeat(32), "02".repeat(32), own),
            ChannelReseal.resealKeys(rows, 2L, setOf(own.uppercase())),
        )
        assertTrue(ChannelReseal.allSeen(rows, setOf(own)))
        assertFalse(ChannelReseal.allSeen(rows.take(2), setOf(own)))
    }

    @Test
    fun `the publishing device registers the account's key once`() = runBlocking {
        val state = boundState()
        state.addChannel(handle())
        state.setKeys(chan, ChannelPublisherKeys(seed = ByteArray(32) { 5 }, signingKeySeed = ByteArray(32) { 6 }))
        state.addChannel(handle(id = other))
        state.setSubscriberKeys(other, ChannelSubscriberKeys(secret, ByteArray(32) { 1 }))
        val port = FakePort(emptyList(), account)
        val stores = FakeStores()
        ChannelSubscriptionSync.reconcileLocked(state, port, stores)
        ChannelSubscriptionSync.registerAccountKeys(state, stores)
        // Only the channel this device publishes; a subscriber's own subscribe
        // registered its key already.
        assertEquals(listOf(chan), stores.registerCalls)
        ChannelSubscriptionSync.reconcileLocked(state, port, stores)
        ChannelSubscriptionSync.registerAccountKeys(state, stores)
        assertEquals(listOf(chan), stores.registerCalls)
    }

    @Test
    fun `a channel removed with Remove from this device is never re-added or written`() {
        val orphans = orphanStore()
        val state = boundState(orphans)
        state.addChannel(handle(role = ChannelAccountRole.OWNER))
        state.setSubscriberKeys(chan, ChannelSubscriberKeys(secret, ByteArray(32) { 1 }))
        state.removeOrphanedFromDevice(chan)
        val port = FakePort(listOf(register(role = ChannelAccountRole.OWNER)), account)
        val stores = FakeStores()
        ChannelSubscriptionSync.reconcileLocked(state, port, stores)
        assertTrue(state.channels.value.isEmpty())
        assertTrue(port.writes.isEmpty())
        // The record outlives a relaunch.
        val again = boundState(orphans)
        ChannelSubscriptionSync.reconcileLocked(again, port, stores)
        assertTrue(again.channels.value.none { it.id == chan })
    }

    @Test
    fun `a channel orphaned by a key reset is left alone by the register`() {
        val orphans = orphanStore().apply { add(account, setOf(chan)) }
        val state = boundState(orphans)
        state.addChannel(handle(addedAtMs = 1uL))
        state.setSubscriberKeys(chan, ChannelSubscriberKeys(ByteArray(32) { 3 }, ByteArray(32) { 4 }))
        // A live register with another key, and nothing held: neither adopted nor written.
        val port = FakePort(listOf(register(hlc = 9_000_000uL)), account)
        val stores = FakeStores()
        ChannelSubscriptionSync.reconcileLocked(state, port, stores)
        assertArrayEquals(ByteArray(32) { 3 }, state.subscriberKeysByChannel.value[chan]!!.secret)
        assertTrue(port.writes.isEmpty())
        assertTrue(stores.retired.isEmpty())
        // A leave does not drop it either.
        val leftPort = FakePort(listOf(register(hlc = 9_000_000uL, subscribed = false, secret = ByteArray(0))), account)
        ChannelSubscriptionSync.reconcileLocked(state, leftPort, stores)
        assertTrue(stores.dropped.isEmpty())
        assertEquals(chan, state.channels.value.single().id)
    }

    @Test
    fun `the let-in pass never removes the account's registered reader key`() {
        val prefs = RuntimeEnvironment.getApplication().getSharedPreferences("second-device-reader-keys", Context.MODE_PRIVATE)
        val accountKey = "ee".repeat(32)
        ChannelAccountReaderKey.markRegistered(prefs, account, chan, accountKey)
        val registered = ChannelAccountReaderKey.registeredKeyHex(prefs, account, chan)
        assertEquals(accountKey, registered)
        // Above the mark, no valid ticket, on the device that set the mark:
        // a stranger's row is removed, the account's own reader key is not,
        // even when this device's local key is another one.
        val row = ChannelSubscriberRows.Row(accountKey.uppercase(), inviteKeyHex = null, seq = 9)
        val stranger = ChannelSubscriberRows.Row("bb".repeat(32), inviteKeyHex = null, seq = 10)
        val local = "cc".repeat(32)
        assertEquals(
            ChannelSubscriberRows.LetIn.LEAVE,
            ChannelSubscriberRows.letInVerdict(row, false, 5L, true, local, registered),
        )
        assertEquals(
            ChannelSubscriberRows.LetIn.REMOVE,
            ChannelSubscriberRows.letInVerdict(stranger, false, 5L, true, local, registered),
        )
        assertNull(ChannelAccountReaderKey.registeredKeyHex(prefs, account, other))
    }
}
