// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** ADR-0058 section 3 — the channel-post log's merge and per-channel sync. */
class FeedChannelPostLogTest {
    private val now = 1_800_000_000_000L
    private val id = "0123456789abcdef0123456789abcdef"

    private fun post(cursor: Long, ts: Long = now, text: String = "p$cursor") = PersistedChannelPost(cursor, ts, text)

    @Test
    fun mergeDeduplicatesKeepsTheLaterCopyAndNeverMovesTheCursorBack() {
        val slot = ChannelPostSlot(cursor = 5, posts = listOf(post(1), post(2, text = "old")))
        val merged = mergeChannelPosts(slot, listOf(post(2, text = "new"), post(3)), cursor = 3, nowMs = now)
        assertEquals(5L, merged.cursor)
        assertEquals(listOf(1L, 2L, 3L), merged.posts.map { it.cursor })
        assertEquals("new", merged.posts[1].text)
    }

    @Test
    fun mergeDropsPostsPastRetentionAndKeepsTheNewestUpToTheCap() {
        val old = post(1, ts = now - CHANNEL_POST_RETENTION_MS - 1)
        val many = (2L..(CHANNEL_POST_CAP + 10L)).map { post(it) }
        val merged = mergeChannelPosts(null, listOf(old) + many, cursor = many.last().cursor, nowMs = now)
        assertEquals(CHANNEL_POST_CAP, merged.posts.size)
        assertEquals(many.last().cursor, merged.posts.last().cursor)
        assertEquals(11L, merged.posts.first().cursor)
    }

    @Test
    fun sanitizeRefusesAnUnknownVersionAndDropsBadChannelsAlone() {
        assertNull(sanitizeChannelPostLog(ChannelPostLog(version = 2)))
        val log = sanitizeChannelPostLog(
            ChannelPostLog(
                channels = mapOf(
                    id to ChannelPostSlot(3, listOf(post(3), post(0), post(1))),
                    "not-hex" to ChannelPostSlot(1, emptyList()),
                ),
            ),
        )!!
        assertEquals(setOf(id), log.channels.keys)
        assertEquals(listOf(1L, 3L), log.channels.getValue(id).posts.map { it.cursor })
    }

    private class FakeDeps(
        val pages: List<List<ChannelPostWire>>,
        val outcomes: Map<String, PostOutcome>,
    ) : ChannelSyncDeps {
        var calls = 0
        override suspend fun fetchPage(channelIdHex: String, after: Long, limit: Int): ChannelPostPage {
            val page = pages.getOrNull(calls++) ?: emptyList()
            return ChannelPostPage(page.filter { it.cursor > after })
        }

        override suspend fun decode(channelIdHex: String, payloadB64: String): PostOutcome =
            outcomes[payloadB64] ?: PostOutcome.Skip
    }

    private fun wire(c: Long) = ChannelPostWire(c, now, "w$c")

    @Test
    fun syncKeepsTextSkipsOthersAndStopsTheCursorAtAPostThatMustWait() = runTest {
        val deps = FakeDeps(
            pages = listOf(listOf(wire(1), wire(2), wire(3), wire(4))),
            outcomes = mapOf(
                "w1" to PostOutcome.Text("hello"),
                "w2" to PostOutcome.Skip,
                "w3" to PostOutcome.Wait,
                "w4" to PostOutcome.Text("after"),
            ),
        )
        val slot = syncChannelSlot(id, null, deps, now)
        assertEquals(2L, slot.cursor)
        assertEquals(listOf("hello"), slot.posts.map { it.text })
    }

    @Test
    fun syncStopsAfterAShortPage() = runTest {
        val deps = FakeDeps(pages = listOf(listOf(wire(1)), listOf(wire(2))), outcomes = emptyMap())
        val slot = syncChannelSlot(id, null, deps, now)
        assertEquals(1, deps.calls)
        assertEquals(1L, slot.cursor)
    }

    @Test
    fun syncFetchesAtMostTheMaxPages() = runTest {
        val full = (1..CHANNEL_POST_MAX_PAGES + 2).map { p ->
            (1..CHANNEL_POST_PAGE).map { i -> wire(((p - 1) * CHANNEL_POST_PAGE + i).toLong()) }
        }
        val deps = FakeDeps(pages = full, outcomes = emptyMap())
        val slot = syncChannelSlot(id, null, deps, now)
        assertEquals(CHANNEL_POST_MAX_PAGES, deps.calls)
        assertEquals((CHANNEL_POST_MAX_PAGES * CHANNEL_POST_PAGE).toLong(), slot.cursor)
    }
}
