// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/** `docs/ux/feed.md` sections 4.1 and 5: icons, hidden, divider, pill, channel read state. */
class FeedReadingTest {
    private val group = "ab".repeat(32)
    private val channel = "cd".repeat(16)
    private val otherChannel = "ef".repeat(16)

    private fun source(id: String, kind: FeedSourceKind = FeedSourceKind.GROUP) =
        FeedSource(kind, id, "t-$id", muted = false, audience = 3)

    // ----- Icons -----

    @Test
    fun iconsAreTwelveInSpecOrderAndUnknownDrawsTheDefault() {
        assertEquals(12, FeedIcon.entries.size)
        assertEquals(FeedIcon.FEED, FeedIcon.of(0u))
        assertEquals(FeedIcon.LIGHTNING, FeedIcon.of(3u))
        assertEquals(FeedIcon.SPEECH_BUBBLE, FeedIcon.of(9u))
        assertEquals(FeedIcon.FLAG, FeedIcon.of(11u))
        assertEquals(FeedIcon.FEED, FeedIcon.of(12u))
        assertEquals(FeedIcon.FEED, FeedIcon.of(255u))
        for (icon in FeedIcon.entries) assertEquals(icon, FeedIcon.of(icon.index))
    }

    // ----- Hidden from feeds -----

    @Test
    fun hiddenConversationsLeaveTheSourcesBeforeRanking() {
        val sources = listOf(source(group), source(channel, FeedSourceKind.CHANNEL), source(otherChannel, FeedSourceKind.CHANNEL))
        val r = hideFromFeeds(FeedReading(), channel)
        assertEquals(listOf(group, otherChannel), withoutHidden(sources, r.hidden).map { it.idHex })
        assertEquals(listOf(channel), hiddenSources(sources, r.hidden).map { it.idHex })
        // Undo / Show again puts it back.
        val back = showInFeedsAgain(r, channel)
        assertEquals(sources, withoutHidden(sources, back.hidden))
        assertTrue(hiddenSources(sources, back.hidden).isEmpty())
    }

    @Test
    fun hidingIsIdempotentAndRefusesAMalformedId() {
        val r = hideFromFeeds(FeedReading(), group)
        assertSame(r, hideFromFeeds(r, group))
        assertSame(r, hideFromFeeds(r, "not-hex"))
        assertSame(r, hideFromFeeds(r, "abc"))
    }

    @Test
    fun hiddenFilteringKeepsTheBuiltInputFreeOfTheConversation() {
        val sources = withoutHidden(listOf(source(group), source(channel, FeedSourceKind.CHANNEL)), setOf(group))
        val input = buildFeedInput(
            FeedContext(
                sources = sources,
                groupMessages = mapOf(
                    group to listOf(FeedMessage(5, "incoming", "hi", createdAt = "2026-09-30T10:00:00Z")),
                ),
                channelPosts = mapOf(
                    channel to ChannelPostSlot(9, listOf(PersistedChannelPost(9, 1_759_226_400_000, "post"))),
                ),
                ownChannels = emptySet(),
                ownUsername = null,
                contactKeysB64 = emptySet(),
                mentions = { _, _ -> false },
                nowMs = 1_759_230_000_000,
            ),
        )
        assertEquals(listOf(channel), input.refs.map { it.source.idHex })
    }

    // ----- "New" divider -----

    @Test
    fun noDividerOnTheFirstVisitOrWhenNothingIsNew() {
        val keys = listOf("a", "b", "c")
        assertNull(newDividerIndex(keys, null))
        val seen = lookedSnapshot(keys).toSet()
        assertNull(newDividerIndex(keys, seen))
        assertNull(newDividerIndex(emptyList(), seen))
    }

    @Test
    fun dividerSitsAboveTheFirstItemThatWasNotThere() {
        val before = lookedSnapshot(listOf("a", "b", "c")).toSet()
        assertEquals(0, newDividerIndex(listOf("x", "a", "b", "c"), before))
        // A ranked feed can place a new item further down.
        assertEquals(2, newDividerIndex(listOf("a", "b", "y", "c", "z"), before))
    }

    @Test
    fun lookedIsPerFeedBoundedAndPrunable() {
        val many = (0 until 1_000).map { "k$it" }
        var r = withLooked(FeedReading(), "LATEST", many)
        assertEquals(LOOKED_KEYS_PER_FEED, r.looked.getValue("LATEST").size)
        r = withLooked(r, "custom:abcdefgh", listOf("a"))
        assertEquals(setOf("LATEST", "custom:abcdefgh"), r.looked.keys)
        assertSame(r, withLooked(r, "bad selection!", listOf("a")))
        assertEquals(setOf("LATEST"), pruneLooked(r, setOf("LATEST")).looked.keys)
        assertEquals(itemKeyHash("a"), itemKeyHash("a"))
        assertFalse(itemKeyHash("a") == itemKeyHash("b"))
    }

    // ----- "Show N new" -----

    @Test
    fun pillCountsOnlyItemsTheListDoesNotShow() {
        assertEquals(0, newItemCount(null, listOf("a")))
        assertEquals(0, newItemCount(listOf("a"), null))
        assertEquals(0, newItemCount(listOf("a", "b"), listOf("b", "a")))
        assertEquals(2, newItemCount(listOf("a", "b"), listOf("x", "a", "y", "b")))
        // An item that dropped out of the feed is not "new".
        assertEquals(1, newItemCount(listOf("a", "b"), listOf("x", "a")))
    }

    // ----- Channel read state -----

    @Test
    fun channelCountIsPostsNewerThanTheNewestRead() {
        val cursors = listOf(3L, 5L, 8L, 13L)
        // No marker yet: about to be seeded at the newest held, so no count.
        assertEquals(0, channelUnreadCount(cursors, null, own = false))
        assertEquals(4, channelUnreadCount(cursors, 0L, own = false))
        assertEquals(2, channelUnreadCount(cursors, 5L, own = false))
        // Reading the newest makes it zero, whatever was skipped.
        assertEquals(0, channelUnreadCount(cursors, 13L, own = false))
        // A channel this device publishes to counts none.
        assertEquals(0, channelUnreadCount(cursors, null, own = true))
    }

    @Test
    fun theMarkerNeverMovesBackwards() {
        var r = advanceChannelRead(FeedReading(), channel, 8)
        assertEquals(8L, r.channelRead[channel])
        assertSame(r, advanceChannelRead(r, channel, 5))
        assertSame(r, advanceChannelRead(r, channel, 8))
        r = advanceChannelRead(r, channel, 13)
        assertEquals(13L, r.channelRead[channel])
        assertSame(r, advanceChannelRead(r, "zz", 20))
        assertSame(r, advanceChannelRead(r, otherChannel, 0))
        assertTrue(isChannelPostUnread(r, channel, 14, own = false))
        assertFalse(isChannelPostUnread(r, channel, 13, own = false))
        assertFalse(isChannelPostUnread(r, channel, 14, own = true))
        assertTrue(isChannelPostUnread(r, otherChannel, 1, own = false))
    }

    @Test
    fun aFreshChannelStartsAtTheNewestPostHeldAndOnlyLaterPostsCount() {
        val seeded = seedChannelRead(FeedReading(), channel, 13)
        assertEquals(13L, seeded.channelRead[channel])
        assertEquals(0, channelUnreadCount(listOf(3L, 8L, 13L), seeded.channelRead[channel], own = false))
        assertEquals(1, channelUnreadCount(listOf(3L, 8L, 13L, 21L), seeded.channelRead[channel], own = false))
        // An existing marker is never re-seeded.
        assertSame(seeded, seedChannelRead(seeded, channel, 40))
        // A channel subscribed with nothing held starts at zero: every post
        // that arrives after counts.
        val empty = seedChannelRead(FeedReading(), otherChannel, 0)
        assertEquals(0L, empty.channelRead[otherChannel])
        assertEquals(2, channelUnreadCount(listOf(4L, 6L), empty.channelRead[otherChannel], own = false))
        assertEquals(empty, decodeFeedReading(encodeFeedReading(empty)))
    }

    // ----- Codec -----

    @Test
    fun codecRoundTripsAndDropsMalformedEntriesAlone() {
        val r = FeedReading(
            hidden = setOf(group, channel),
            looked = mapOf("LATEST" to listOf(1L, 2L)),
            channelRead = mapOf(channel to 7L),
        )
        assertEquals(r, decodeFeedReading(encodeFeedReading(r)))
        val wire = FeedReadingWire(
            hidden = listOf(group, "nope", "abc"),
            looked = mapOf("LATEST" to listOf(1L), "bad key!" to listOf(2L)),
            channelRead = mapOf(channel to 4L, otherChannel to -1L, "short" to 3L),
        )
        val decoded = decodeFeedReading(wire)
        assertEquals(setOf(group), decoded.hidden)
        assertEquals(setOf("LATEST"), decoded.looked.keys)
        assertEquals(mapOf(channel to 4L), decoded.channelRead)
        assertEquals(FeedReading(), decodeFeedReading(FeedReadingWire(version = 99, hidden = listOf(group))))
        assertEquals(FeedReading(), decodeFeedReading(null))
    }
}
