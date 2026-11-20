// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0063 section 3 — the channel-post log's merge and per-channel sync. */
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
        assertNull(sanitizeChannelPostLog(ChannelPostLog(version = 5)))
        // Version 1 passed caption-less attachment posts over: refetched.
        assertNull(sanitizeChannelPostLog(ChannelPostLog(version = 1)))
        // Version 2 kept no attachment references, so the feed could not draw
        // a channel's media (base scenario 32): refetched.
        assertNull(sanitizeChannelPostLog(ChannelPostLog(version = 2)))
        // Version 3 kept no content post (ADR-0092): refetched.
        assertNull(sanitizeChannelPostLog(ChannelPostLog(version = 3)))
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
    fun aLogStoredWithNoVersionIsTheOldOneAndIsFetchedAgain() {
        // Live on the Pixel 7a (2026-11-12): the store's JSON leaves default
        // values out, so the version-1 log carried no version and was read as
        // version 2; its cursor stood past the caption-less posts, so they
        // were never filed and the row kept the first post.
        val v1OnDisk = """{"channels":{"$id":{"cursor":4,"posts":[{"cursor":1,"serverTsMs":$now,"text":"First post"}]}}}"""
        assertNull(decodeChannelPostLog(v1OnDisk))
        assertNull(decodeChannelPostLog("""{"version":1,"channels":{}}"""))
        assertNull(decodeChannelPostLog("not json"))
    }

    @Test
    fun aDroppedLogStillTellsHowFarItHadLookedPerChannel() {
        val v1OnDisk = """{"channels":{"$id":{"cursor":4,"posts":[{"cursor":1,"serverTsMs":$now,"text":"First post"}]},""" +
            """"not-hex":{"cursor":9},"ffffffffffffffffffffffffffffffff":{"posts":[{"cursor":7,"serverTsMs":$now,"text":"x"}]}}}"""
        assertEquals(
            mapOf(id to 4L, "ffffffffffffffffffffffffffffffff" to 7L),
            droppedLogCursors(v1OnDisk),
        )
        assertEquals(emptyMap<String, Long>(), droppedLogCursors("not json"))
    }

    @Test
    fun aStoredLogNamesItsVersionAndReadsBackWithItsAttachments() {
        val log = ChannelPostLog(
            channels = mapOf(
                id to ChannelPostSlot(
                    3,
                    listOf(
                        post(1),
                        PersistedChannelPost(2, now, "", listOf(PersistedChannelPostMedia("image/gif", "gif"))),
                        // Version 3: the reference the feed draws the photo by.
                        PersistedChannelPost(
                            3,
                            now,
                            "",
                            listOf(
                                PersistedChannelPostMedia(
                                    "image/jpeg",
                                    null,
                                    me.lakko.app.chat.PersistedAttachment(
                                        blobIdB64 = "AAEC",
                                        keyB64 = "AwQF",
                                        nonceB64 = "BgcI",
                                        mime = "image/jpeg",
                                        size = 99uL,
                                        name = "p.jpg",
                                        width = 640u,
                                        height = 480u,
                                    ),
                                ),
                            ),
                        ),
                    ),
                ),
            ),
        )
        val text = encodeChannelPostLog(log)
        assertTrue(text.contains("\"version\":$CHANNEL_POST_LOG_VERSION"))
        assertEquals(log, decodeChannelPostLog(text))
        assertEquals(ChannelPostLog(), decodeChannelPostLog(encodeChannelPostLog(ChannelPostLog())))
    }

    @Test
    fun syncKeepsAnAttachmentPostWithNoTextAndItsKinds() = runTest {
        // Base scenario 22 (2026-11-12): a photo, a file and a GIF posted
        // without a caption were passed over, so the row kept the first post.
        val deps = FakeDeps(
            pages = listOf(listOf(wire(1), wire(2))),
            outcomes = mapOf(
                "w1" to PostOutcome.Text("First post"),
                "w2" to PostOutcome.Text("", listOf(PersistedChannelPostMedia(mime = "image/gif", kind = "gif"))),
            ),
        )
        val slot = syncChannelSlot(id, null, deps, now)
        assertEquals(listOf(1L, 2L), slot.posts.map { it.cursor })
        assertEquals(listOf(PersistedChannelPostMedia("image/gif", "gif")), slot.posts.last().media)
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
