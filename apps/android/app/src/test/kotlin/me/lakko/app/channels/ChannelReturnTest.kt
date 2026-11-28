// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import me.lakko.app.account.AccountStoreGate
import me.lakko.app.auth.MarkerStorage
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/** ADR-0101 section 7 - who sends which channel return, and what the reset
 *  device does with one. Mirror of `apps/web/src/channels/channel-return.test.ts`
 *  (the wire cases run in the Rust and web suites; the host JVM has no core). */
class ChannelReturnTest {

    private class MapStorage : MarkerStorage {
        val rows = HashMap<String, String>()
        override fun get(key: String): String? = rows[key]
        override fun set(key: String, value: String) {
            rows[key] = value
        }
        override fun remove(key: String) {
            rows.remove(key)
        }
    }

    @Before
    fun setUp() = AccountStoreGate.resetForTest()

    @After
    fun tearDown() = AccountStoreGate.resetForTest()

    private fun action(
        verified: Boolean = true,
        role: ChannelReturnRoleName,
        hasInvitation: Boolean = true,
        alreadyInRole: Boolean = false,
    ) = channelReturnAction(verified, role, hasInvitation, alreadyInRole)

    @Test
    fun `an unverified return is shown when it carries an invitation, dropped otherwise, never acted on`() {
        for (role in ChannelReturnRoleName.entries) {
            assertEquals(ChannelReturnAction.SHOW_INVITATION, action(verified = false, role = role))
            assertEquals(ChannelReturnAction.DROP, action(verified = false, role = role, hasInvitation = false))
        }
    }

    @Test
    fun `a subscriber subscribes, or only takes the seeds when it already reads`() {
        assertEquals(ChannelReturnAction.SUBSCRIBE, action(role = ChannelReturnRoleName.SUBSCRIBER))
        assertEquals(ChannelReturnAction.ADD_SEEDS, action(role = ChannelReturnRoleName.SUBSCRIBER, alreadyInRole = true))
        assertEquals(ChannelReturnAction.DROP, action(role = ChannelReturnRoleName.SUBSCRIBER, hasInvitation = false))
    }

    @Test
    fun `a publisher accepts its publisher invitation unless it publishes already`() {
        assertEquals(ChannelReturnAction.ACCEPT_PUBLISHER, action(role = ChannelReturnRoleName.PUBLISHER))
        assertEquals(ChannelReturnAction.ADD_SEEDS, action(role = ChannelReturnRoleName.PUBLISHER, alreadyInRole = true))
        assertEquals(ChannelReturnAction.ADD_SEEDS, action(role = ChannelReturnRoleName.PUBLISHER, hasInvitation = false))
    }

    @Test
    fun `there is no owner return - an owner takes its channels back from its own list`() {
        // ADR-0101 section 7, rewritten 2026-11-26.
        assertEquals(
            listOf(ChannelReturnRoleName.SUBSCRIBER, ChannelReturnRoleName.PUBLISHER),
            ChannelReturnRoleName.entries.toList(),
        )
        assertTrue(ChannelReturnAction.entries.none { it.name.contains("OWNER") })
    }

    private val superseded = mapOf("OLD_SUB" to "acctSub", "OLD_PUB" to "acctPub", "OLD_OWNER" to "acctOwner")

    private fun card(k: String) = ReturnCard(k, "sp-$k")

    private fun ch(
        ownRole: ReturnOwnRole = ReturnOwnRole.OWNER,
        cards: List<ReturnCard> = listOf(card("OLD_SUB"), card("OLD_PUB"), card("OLD_OWNER"), card("ME"), card("OTHER")),
        ownerKeyB64: String? = "ME",
        publishers: List<String> = listOf("OLD_PUB"),
    ) = ReturnChannel("c1", ownRole, cards, ownerKeyB64, publishers)

    @Test
    fun `an owner sends subscriber returns and admits publishers`() {
        val out = channelReturnsFor(listOf(ch(cards = listOf(card("OLD_SUB"), card("OLD_PUB"), card("ME")))), superseded, setOf("ME"))
        assertEquals(
            listOf(ChannelReturnRoleName.SUBSCRIBER to "acctSub", ChannelReturnRoleName.PUBLISHER to "acctPub"),
            out.map { it.role to it.accountIdB64 },
        )
    }

    @Test
    fun `a co-publisher sends subscriber returns, no publisher return, and nothing for a reset owner`() {
        val out = channelReturnsFor(listOf(ch(ownRole = ReturnOwnRole.PUBLISHER, ownerKeyB64 = "OLD_OWNER")), superseded, setOf("ME"))
        assertEquals(
            listOf(ChannelReturnRoleName.SUBSCRIBER to "acctSub"),
            out.map { it.role to it.accountIdB64 },
        )
    }

    @Test
    fun `a subscriber sends nothing, with or without the owner's card`() {
        assertTrue(
            channelReturnsFor(listOf(ch(ownRole = ReturnOwnRole.SUBSCRIBER, ownerKeyB64 = "OLD_OWNER")), superseded, setOf("ME")).isEmpty(),
        )
        assertTrue(
            channelReturnsFor(
                listOf(ch(ownRole = ReturnOwnRole.SUBSCRIBER, ownerKeyB64 = "OLD_OWNER", cards = listOf(card("OLD_SUB")))),
                superseded,
                setOf("ME"),
            ).isEmpty(),
        )
    }

    @Test
    fun `nothing for this account's own keys`() {
        assertTrue(
            channelReturnsFor(listOf(ch(cards = listOf(card("OLD_SUB"), card("OLD_PUB")))), superseded, setOf("ME", "OLD_SUB", "OLD_PUB")).isEmpty(),
        )
    }

    @Test
    fun `each return once per accepted generation`() {
        val planned = channelReturnsFor(listOf(ch(cards = listOf(card("OLD_SUB"), card("OLD_PUB")))), superseded, setOf("ME"))
        val sent = mapOf(sentReturnKey(planned[0]) to 3L)
        val gen: (String) -> Long? = { a -> if (a == "acctSub") 3L else if (a == "acctPub") null else 1L }
        assertTrue(unsentReturns(planned, sent, gen).isEmpty())
        assertEquals(2, unsentReturns(planned, sent) { 4L }.size)
        assertEquals("c1|subscriber|acctSub", sentReturnKey(planned[0]))
    }

    @Test
    fun `a return carries the newest 64 seeds, ascending, one per epoch`() {
        val seeds = (0 until 70).map { i -> HeldSeed(i.toUInt(), ByteArray(32) { i.toByte() }) } +
            HeldSeed(5u, ByteArray(31))
        val out = newestSeeds(seeds)
        assertEquals(CHANNEL_RETURN_MAX_SEEDS, out.size)
        assertEquals(6u, out.first().epoch)
        assertEquals(69u, out.last().epoch)
        assertEquals(6.toByte(), out.first().seed[0])
    }

    @Test
    fun `held returns are kept once each, bounded, and dropped when handled`() {
        val store = HeldReturnsStore(MapStorage())
        store.hold("a", "x")
        store.hold("a", "x")
        store.hold("a", "y")
        assertEquals(listOf("x", "y"), store.read("a"))
        store.drop("a", listOf("x"))
        assertEquals(listOf("y"), store.read("a"))
        for (i in 0 until HeldReturnsStore.MAX_HELD_RETURNS + 3) store.hold("b", i.toString())
        assertEquals(HeldReturnsStore.MAX_HELD_RETURNS, store.read("b").size)
        assertEquals("3", store.read("b").first())
    }

    @Test
    fun `what was sent round-trips per account`() {
        val store = SentReturnsStore(MapStorage())
        store.write("a", mapOf("c|subscriber|x" to 2L))
        assertEquals(mapOf("c|subscriber|x" to 2L), store.read("a"))
        assertTrue(store.read("b").isEmpty())
    }

    @Test
    fun `the recovery key registrations owed are every owned channel not recorded, sorted`() {
        assertEquals(listOf("a", "b"), channelsOwingRecoveryKey(listOf("b", "a", "c"), setOf("c")))
        assertTrue(channelsOwingRecoveryKey(emptyList(), emptySet()).isEmpty())
        val store = RecoveryRegisteredStore(MapStorage())
        store.mark("acct", "b")
        store.mark("acct", "a")
        assertEquals(setOf("a", "b"), store.read("acct"))
    }

    // Live walk 2026-11-26, D1 - the owner's and the publishers' own cards.

    @Test
    fun `every channel this device publishes owes its own card, owner and publisher alike`() {
        val publishes = setOf("owned", "copublished")
        val due = channelsOwingPublishingCard(
            channels = listOf("owned", "read-only", "copublished", "owned"),
            publishesHere = { it in publishes },
            removed = { false },
        )
        assertEquals(listOf("owned", "copublished"), due)
    }

    @Test
    fun `a removed channel owes no card, and a device that only reads owes none app-wide`() {
        val due = channelsOwingPublishingCard(
            channels = listOf("owned", "gone", "read-only"),
            publishesHere = { it != "read-only" },
            removed = { it == "gone" },
        )
        assertEquals(listOf("owned"), due)
    }

    @Test
    fun `an owner card in the list plans nothing on a subscriber device`() {
        // The reset owner is not returned by anyone: its own device takes
        // the channel back with the recovery key its list holds.
        val owner = "T1dORVJfT0xEX0tFWQ=="
        val withCard = ReturnChannel(
            idHex = "c1",
            ownRole = ReturnOwnRole.SUBSCRIBER,
            cards = listOf(ReturnCard(owner, "c3ViLW93bmVy")),
            ownerKeyB64 = owner,
            publisherKeysB64 = emptyList(),
        )
        assertTrue(channelReturnsFor(listOf(withCard), mapOf(owner to "acct"), setOf("bWU=")).isEmpty())
    }
}
