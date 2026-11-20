// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discover

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import me.lakko.app.l10n.UiText

/**
 * ADR-0059 S4 Android — the two runtimes behind their seams: the
 * shared-channels repository (switch, send, receive, growth re-send, the
 * debounced re-send on a change of the followed set) and the Discover model
 * (the daily refresh keeps picked buckets only, an all-failed refresh reads as
 * unavailable, the handle check before subscribing).
 */
@OptIn(ExperimentalCoroutinesApi::class)
class DiscoverRuntimeTest {

    private fun id(n: Int) = n.toString().padStart(32, '0')

    // --- shared channels repository ---------------------------------------

    private class Harness(scope: TestScope) {
        val sent = mutableListOf<Pair<String, SharedList>>()
        var stored: SharedChannelsRow = SharedChannelsRow()
        var live = true
        var failSend = false
        /** ADR-0091 walk 3, W3-R2 — every send answers "held". */
        var heldSend = false
        var shareable: List<ShareableChannel>? = emptyList()
        val repo = SharedChannelsRepository(
            account = { "acct" },
            broadcast = { g, envelope ->
                if (failSend) throw IllegalStateException("network")
                if (heldSend) {
                    false
                } else {
                    sent += g to decodeFake(envelope)
                    true
                }
            },
            isLive = { live },
            scope = scope,
            compose = ::encodeFake,
            isNewer = { a, b -> a > b },
            load = { stored },
            save = { _, r -> stored = r; true },
            log = { _, _ -> },
        ).also { it.shareable = { shareable } }

        companion object {
            // A stand-in for the core codec: the list round-trips through the
            // row codec's JSON, which is enough to see what was sent.
            fun encodeFake(l: SharedList): ByteArray =
                encodeSharedChannelsRow(SharedChannelsRow(sent = mapOf("x" to l))).toByteArray()

            fun decodeFake(b: ByteArray): SharedList = decodeSharedChannelsRow(String(b)).sent.getValue("x")
        }
    }

    private val books = ShareableChannel(id(1), subscribed = true, listed = true, handle = "books", name = "Books")

    @Test
    fun `turning the switch on sends the listed channels, off sends an empty list`() = runTest {
        val h = Harness(this)
        h.shareable = listOf(books)
        h.repo.setSharing("g", true)
        assertTrue(h.repo.isSharing("g"))
        assertEquals(listOf(SharedChannel(id(1), "books", "Books")), h.sent.single().second.channels)
        h.repo.setSharing("g", false)
        assertFalse(h.repo.isSharing("g"))
        assertEquals(emptyList<SharedChannel>(), h.sent.last().second.channels)
        assertTrue(h.sent.last().second.updatedAtMs > h.sent.first().second.updatedAtMs)
        assertEquals(setOf<String>(), h.stored.shareOn)
    }

    @Test
    fun `a failed send leaves the switch where it was`() = runTest {
        val h = Harness(this)
        h.failSend = true
        runCatching { h.repo.setSharing("g", true) }
        assertFalse(h.repo.isSharing("g"))
        h.live = false
        h.failSend = false
        assertTrue(runCatching { h.repo.setSharing("g", true) }.isFailure)
        assertTrue(h.sent.isEmpty())
    }

    @Test
    fun `a held send records nothing, so the next change re-send sends the list`() = runTest {
        // ADR-0091 walk 3, W3-R2 — the device may be behind in the group.
        val h = Harness(this)
        h.shareable = listOf(books)
        h.heldSend = true
        h.repo.setSharing("g", true)
        assertTrue(h.sent.isEmpty())
        assertTrue(h.stored.sent["g"] == null)
        h.heldSend = false
        h.repo.followedSetChanged()
        advanceUntilIdle()
        assertEquals(listOf(SharedChannel(id(1), "books", "Books")), h.sent.single().second.channels)
    }

    @Test
    fun `received lists are filed per member, newest wins, in any order`() = runTest {
        val h = Harness(this)
        h.repo.receive("g", "m", SharedList(5uL, listOf(SharedChannel(id(2), "news", "News"))))
        h.repo.receive("g", "m", SharedList(3uL, emptyList()))
        h.repo.receive("g", null, SharedList(9uL, emptyList()))
        advanceUntilIdle()
        assertEquals(5uL, h.stored.received["g"]!!["m"]!!.updatedAtMs)
        assertEquals(setOf("m"), h.stored.received["g"]!!.keys)
    }

    @Test
    fun `a change of the followed set re-sends once after the debounce, only where it differs`() = runTest {
        val h = Harness(this)
        h.shareable = listOf(books)
        h.repo.setSharing("g", true)
        h.sent.clear()
        h.repo.followedSetChanged()
        advanceUntilIdle()
        assertTrue("unchanged set must not re-send", h.sent.isEmpty())
        h.shareable = listOf(books, ShareableChannel(id(2), true, true, "tech", "Tech"))
        h.repo.followedSetChanged()
        h.repo.followedSetChanged()
        advanceTimeBy(2_000)
        assertTrue(h.sent.isEmpty())
        advanceUntilIdle()
        assertEquals(1, h.sent.size)
        assertEquals(2, h.sent.single().second.channels.size)
        h.sent.clear()
        h.shareable = null
        h.repo.followedSetChanged()
        advanceUntilIdle()
        assertTrue("a channel list not loaded yet never withdraws", h.sent.isEmpty())
    }

    @Test
    fun `forgetting a group drops its lists and its switch`() = runTest {
        val h = Harness(this)
        h.shareable = listOf(books)
        h.repo.setSharing("g", true)
        h.repo.receive("g", "m", SharedList(5uL, emptyList()))
        advanceUntilIdle()
        h.repo.forgetGroup("g")
        advanceUntilIdle()
        assertEquals(SharedChannelsRow(), h.stored)
    }

    // --- Discover model ---------------------------------------------------

    private class FakeBackend : DiscoverBackend {
        val asked = mutableListOf<Int>()
        var failAll = false
        var resolved: String? = null
        var prefs = DiscoverPrefs()
        override suspend fun fetchBucket(topic: Int, language: String): List<DirectoryItemWire> {
            asked += topic
            if (failAll) throw IllegalStateException("offline")
            return listOf(DirectoryItemWire(topic.toString().padStart(32, '0'), "L$topic", "10+"))
        }
        override suspend fun fetchListing(channelIdHex: String): DirectoryItemWire? = null
        override suspend fun search(query: String): List<DirectoryItemWire> = emptyList()
        override suspend fun resolveHandle(handle: String): String? = resolved
        override fun decodeListing(listingB64: String): DecodedListing? {
            val t = listingB64.removePrefix("L").toInt()
            return DecodedListing(t.toString().padStart(32, '0'), "h$t", "N$t", "", t, "en")
        }
        override fun topics(): List<Int> = (0 until 24).toList()
        override fun loadPrefs(accountIdB64: String): DiscoverPrefs = prefs
        override fun savePrefs(accountIdB64: String, prefs: DiscoverPrefs) {
            this.prefs = prefs
        }
        override fun searchErrorText(error: Throwable): UiText = UiText("err")
        override fun lookupErrorText(error: Throwable): UiText = UiText("lookup")
    }

    @Test
    fun `a refresh asks eight buckets and keeps only the picked ones`() = runTest(StandardTestDispatcher()) {
        val b = FakeBackend()
        b.prefs = DiscoverPrefs(topics = listOf(2, 4), language = "en")
        val m = DiscoverModel(b, { "acct" }, this, "en", nowMs = { 1_790_000_000_000L })
        m.load()
        m.refresh()
        assertEquals(8, b.asked.size)
        assertEquals(setOf("2:en", "4:en"), b.prefs.buckets.keys)
        assertEquals(listOf(id(2), id(4)).toSet(), m.bucketCards().map { it.channelIdHex }.toSet())
        b.asked.clear()
        m.refresh()
        assertTrue("not due again the same day", b.asked.isEmpty())
    }

    @Test
    fun `a refresh where every bucket failed reads as unavailable and keeps the cache`() = runTest(StandardTestDispatcher()) {
        val b = FakeBackend()
        val cached = mapOf("2:en" to CachedBucket(1, listOf(DirectoryItemWire(id(2), "L2", "10+"))))
        b.prefs = DiscoverPrefs(topics = listOf(2), language = "en", buckets = cached)
        b.failAll = true
        val m = DiscoverModel(b, { "acct" }, this, "en")
        m.load()
        m.refresh(force = true)
        assertTrue(m.unavailable.value)
        assertEquals(cached, b.prefs.buckets)
    }

    @Test
    fun `subscribing checks the handle first and a mismatch removes the card`() = runTest(StandardTestDispatcher()) {
        val b = FakeBackend()
        val m = DiscoverModel(b, { "acct" }, this, "en")
        val card = DirectoryCard(id(1), "books", "Books", "", 1, "en", "10+", CardSource.BUCKET, 0, null)
        var opened: Pair<String, String>? = null
        b.resolved = id(2)
        m.subscribe(card) { i, h -> opened = i to h }
        assertEquals(null, opened)
        assertTrue(id(1) in m.gone.value)
        assertEquals(DiscoverCopy.GONE, m.notice.value)
        b.resolved = id(1)
        m.subscribe(card) { i, h -> opened = i to h }
        assertEquals(id(1) to "books", opened)
    }

    @Test
    fun `no picked topics, no download`() = runTest(StandardTestDispatcher()) {
        val b = FakeBackend()
        val m = DiscoverModel(b, { "acct" }, this, "en")
        m.load()
        m.refresh(force = true)
        assertTrue(b.asked.isEmpty())
    }
}
