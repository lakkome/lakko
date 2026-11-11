// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0087 section 13 — the channel timeline fold. */
class ChannelTimelineTest {

    private fun post(cursor: Long, text: String) =
        ChannelRow(cursor, cursor * 1000, 1u, ChannelRowContent.Post(text, emptyList()))

    @Test
    fun postsComeOldestFirstWhateverTheFetchOrder() {
        val t = foldChannelTimeline(listOf(post(3, "c"), post(1, "a"), post(2, "b")))
        assertEquals(listOf("a", "b", "c"), t.posts.map { it.text })
        assertFalse(t.earlierUnavailable)
    }

    @Test
    fun theNewestEditWinsAndMarksThePost() {
        val t = foldChannelTimeline(
            listOf(
                post(1, "first"),
                ChannelRow(3, 3000, 1u, ChannelRowContent.Edit(1, "third")),
                ChannelRow(2, 2000, 1u, ChannelRowContent.Edit(1, "second")),
            ),
        )
        assertEquals("third", t.posts.single().text)
        assertTrue(t.posts.single().edited)
    }

    @Test
    fun aDeletedPostLeavesTheTimeline() {
        val t = foldChannelTimeline(
            listOf(post(1, "a"), post(2, "b"), ChannelRow(3, 3000, 1u, ChannelRowContent.Delete(1))),
        )
        assertEquals(listOf(2L), t.posts.map { it.cursor })
    }

    @Test
    fun opsNamingNoKnownPostAreDropped() {
        val t = foldChannelTimeline(
            listOf(
                post(1, "a"),
                ChannelRow(2, 2000, 1u, ChannelRowContent.Edit(9, "x")),
                ChannelRow(3, 3000, 1u, ChannelRowContent.Delete(8)),
            ),
        )
        assertEquals(listOf("a"), t.posts.map { it.text })
        assertFalse(t.posts.single().edited)
    }

    @Test
    fun unreadablePostsBeforeTheFirstReadableOneBecomeTheTopLine() {
        val t = foldChannelTimeline(
            listOf(
                ChannelRow(1, 1000, 0u, ChannelRowContent.Unavailable),
                post(2, "b"),
                ChannelRow(3, 3000, 0u, ChannelRowContent.Unavailable),
            ),
        )
        assertTrue(t.earlierUnavailable)
        assertEquals(listOf(2L, 3L), t.posts.map { it.cursor })
        assertTrue(t.posts.last().unavailable)
    }

    @Test
    fun hiddenRowsShowNothing() {
        val t = foldChannelTimeline(listOf(ChannelRow(1, 1000, 1u, ChannelRowContent.Hidden), post(2, "b")))
        assertEquals(listOf("b"), t.posts.map { it.text })
        assertFalse(t.earlierUnavailable)
    }

    @Test
    fun theHeaderLineFollowsTheFlagAndTheBand() {
        assertEquals("channels.header.private", channelHeaderLineKey(public = false, band = "10k+").first)
        assertEquals("channels.header.public", channelHeaderLineKey(public = true, band = null).first)
        val (key, args) = channelHeaderLineKey(public = true, band = "100+")
        assertEquals("channels.header.public_count", key)
        assertEquals("100+", args["band"])
    }

    @Test
    fun subscriberBandsAreTheDirectorysBands() {
        assertEquals("<10", subscriberBand(7))
        assertEquals("<10", subscriberBand(1))
        assertEquals("10+", subscriberBand(10))
        assertEquals("100+", subscriberBand(450))
        assertEquals("1k+", subscriberBand(1_000))
        assertEquals("10k+", subscriberBand(12_408))
        assertEquals("100k+", subscriberBand(1_000_000))
    }
}
