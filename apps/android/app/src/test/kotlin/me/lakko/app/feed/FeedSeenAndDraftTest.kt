// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import me.lakko.app.l10n.UiText

/** ADR-0063 section 5 (seen) and section 6 (own feeds, sharing). */
class FeedSeenAndDraftTest {
    /** The core's rule, restated for the host suite: the highest id with
     *  every listed id at or below it seen. */
    private val contiguous: (List<Long>, List<Long>) -> Long? = { ids, seen ->
        val s = seen.toSet()
        var best: Long? = null
        for (id in ids.sorted()) {
            if (id !in s) break
            best = id
        }
        best
    }

    private val rows = listOf(
        SeenRow(1, "incoming"),
        SeenRow(2, "outgoing"),
        SeenRow(3, "incoming"),
        SeenRow(4, "incoming"),
    )

    @Test
    fun outOfOrderSeenStaysAboveTheWatermarkUntilTheGapCloses() {
        val first = markSeen(rows, GroupSeenState(through = 1, feedSeen = emptySet()), listOf(4L), contiguous)
        // 2 is own, 3 unseen: the watermark moves to 2 only.
        assertEquals(2L, first.advancedTo)
        assertEquals(setOf(4L), first.state.feedSeen)
        assertEquals(1, first.newlySeen)

        val second = markSeen(rows, first.state, listOf(3L), contiguous)
        assertEquals(4L, second.advancedTo)
        assertEquals(emptySet<Long>(), second.state.feedSeen)
        assertTrue(isSeen(second.state, 4, "incoming"))
    }

    @Test
    fun anIdNamingNoMessageIsIgnoredAndTheWatermarkNeverMovesBack() {
        val r = markSeen(rows, GroupSeenState(through = 4, feedSeen = emptySet()), listOf(99L), contiguous)
        assertNull(r.advancedTo)
        assertEquals(4L, r.state.through)
        assertEquals(0, r.newlySeen)
        assertFalse(isSeen(GroupSeenState(null, emptySet()), 3, "incoming"))
        assertTrue(isSeen(GroupSeenState(null, emptySet()), 3, "outgoing"))
    }

    @Test
    fun readCursorBytesRoundTrip() {
        assertArrayEquals(byteArrayOf(0, 0, 0, 0, 0, 0, 1, 2), readCursorBytes(258))
        assertEquals(258L, readCursorId(readCursorBytes(258)))
        assertNull(readCursorId(ByteArray(3)))
    }

    @Test
    fun refusalCodesBecomeOneSentence() {
        assertEquals("name", refusalCodeOf("name: display name is empty"))
        assertNull(refusalCodeOf("no code here"))
        assertEquals(UiText("feed.problem.no_kinds"), problemText("noKinds"))
        assertEquals(UiText("feed.problem.generic"), problemText(null))
    }

    @Test
    fun aShareLinkCarriesTheRuleInItsFragment() {
        val bytes = byteArrayOf(1, 2, 3, -1, -2)
        val link = shareLink("https://lakko.me/", bytes)
        assertTrue(link.startsWith("https://lakko.me/feed/import#"))
        assertArrayEquals(bytes, bytesFromFragment(link.substringAfter('#')))
        assertArrayEquals(bytes, bytesFromFragment("#" + link.substringAfter('#')))
        assertNull(bytesFromFragment(""))
        assertNull(bytesFromFragment("not base64!"))
    }

    @Test
    fun savedFeedsDecodeLenientlyAndStayBounded() {
        val good = SavedFeedWire("abcdefgh", "AQID")
        val state = decodeFeedState(
            FeedStateWire(feeds = listOf(good, SavedFeedWire("short", "AQID"), good, SavedFeedWire("ijklmnop", "***"))),
        )
        assertEquals(listOf("abcdefgh"), state.feeds.map { it.id })
        assertEquals(0, decodeFeedState(FeedStateWire(version = 9, feeds = listOf(good))).feeds.size)

        var s = FeedState()
        repeat(MAX_SAVED_FEEDS + 3) { s = upsertFeed(s, SavedFeed("feed%05d".format(it), byteArrayOf(1))) }
        assertEquals(MAX_SAVED_FEEDS, s.feeds.size)
        s = upsertFeed(s, SavedFeed("feed00000", byteArrayOf(9)))
        assertEquals(9.toByte(), s.feeds.first().rule[0])
        assertEquals(MAX_SAVED_FEEDS - 1, removeFeed(s, "feed00000").feeds.size)
        assertTrue(newFeedId().matches(Regex("^[A-Za-z0-9_-]{16}$")))
    }
}
