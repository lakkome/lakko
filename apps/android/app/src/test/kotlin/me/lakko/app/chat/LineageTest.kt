// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0073 R.5 - one conversation over several groups (mirror of the web
 *  `lineage` and `pin-bar` segment tables). */
class LineageTest {
    private val lineage = buildLineage(
        listOf(
            LineageEntry("s2", "root", 20UL),
            LineageEntry("s1", "root", 10UL),
            // Written against the parent instead of the root: followed.
            LineageEntry("s3", "s2", 30UL),
            LineageEntry("self", "self", 1UL),
            LineageEntry("x", "y", 1UL),
            LineageEntry("y", "x", 2UL),
        ),
    )

    @Test
    fun `segments are the root then its successors by stamp`() {
        assertEquals(listOf("root", "s1", "s2", "s3"), lineage.segmentsOf("root"))
        assertEquals("root", lineage.conversationOf("s3"))
        assertEquals("root", lineage.conversationOf("root"))
        assertEquals(3, lineage.segmentIndexOf("s3"))
        assertEquals(0, lineage.segmentIndexOf("alone"))
        assertTrue(lineage.isSuccessor("s1"))
        assertFalse(lineage.isSuccessor("root"))
        assertEquals(listOf("alone"), NO_LINEAGE.segmentsOf("alone"))
    }

    @Test
    fun `a register naming itself and a cycle are ignored`() {
        assertEquals("self", lineage.conversationOf("self"))
        assertEquals("x", lineage.conversationOf("x"))
        assertEquals("y", lineage.conversationOf("y"))
    }

    @Test
    fun `ties on the stamp break on the id so every device agrees`() {
        val a = buildLineage(listOf(LineageEntry("b", "r", 5UL), LineageEntry("a", "r", 5UL)))
        assertEquals(listOf("r", "a", "b"), a.segmentsOf("r"))
    }

    @Test
    fun `the head is the last segment this device is active in`() {
        val segments = listOf("root", "s1", "s2")
        fun head(m: Map<String, SegmentMembership>) = headSegment(segments) { m[it] ?: SegmentMembership.ABSENT }
        assertEquals("s1", head(mapOf("root" to SegmentMembership.ACTIVE, "s1" to SegmentMembership.ACTIVE)))
        // A successor not joined yet is not the head.
        assertEquals("root", head(mapOf("root" to SegmentMembership.ACTIVE)))
        assertEquals("s1", head(mapOf("root" to SegmentMembership.ACTIVE, "s1" to SegmentMembership.ACTIVE, "s2" to SegmentMembership.ENDED)))
        assertEquals("s1", head(mapOf("root" to SegmentMembership.ENDED, "s1" to SegmentMembership.ENDED)))
        assertNull(head(emptyMap()))
    }

    @Test
    fun `timeline order is segment then message id`() {
        assertTrue(compareTimeline(TimelinePosition(0, 99), TimelinePosition(1, 1)) < 0)
        assertTrue(compareTimeline(TimelinePosition(1, 2), TimelinePosition(1, 1)) > 0)
        assertEquals(0, compareTimeline(TimelinePosition(2, 5), TimelinePosition(2, 5)))
    }

    @Test
    fun `the read position bytes - eight in the first segment, twelve after`() {
        assertArrayEquals(byteArrayOf(0, 0, 0, 0, 0, 0, 1, 2), encodeReadPosition(TimelinePosition(0, 258)))
        assertArrayEquals(
            byteArrayOf(0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 3),
            encodeReadPosition(TimelinePosition(3, 7)),
        )
        assertArrayEquals(ByteArray(8), encodeReadPosition(TimelinePosition(0, -4)))
        assertEquals(TimelinePosition(0, 258), decodeReadPosition(byteArrayOf(0, 0, 0, 0, 0, 0, 1, 2)))
        assertEquals(TimelinePosition(3, 7), decodeReadPosition(encodeReadPosition(TimelinePosition(3, 7))))
        assertNull(decodeReadPosition(ByteArray(7)))
        assertNull(decodeReadPosition(ByteArray(8) { 0xFF.toByte() }))
        // Bytes between the two forms read as the first segment.
        assertEquals(TimelinePosition(0, 1), decodeReadPosition(byteArrayOf(0, 0, 0, 0, 0, 0, 0, 1, 9, 9)))
    }

    @Test
    fun `what a position means for one segment`() {
        assertEquals(SegmentRead.Nothing, readUpToInSegment(null, 0))
        assertEquals(SegmentRead.Nothing, readUpToInSegment(TimelinePosition(0, 5), 1))
        assertEquals(SegmentRead.UpTo(5), readUpToInSegment(TimelinePosition(1, 5), 1))
        assertEquals(SegmentRead.All, readUpToInSegment(TimelinePosition(2, 5), 1))
    }

    @Test
    fun `one row per conversation`() {
        val plans = planConversations(
            listOf(HeldGroup("s1", true), HeldGroup("alone", true), HeldGroup("root", false), HeldGroup("s3", false)),
            lineage,
        )
        assertEquals(
            listOf(
                ConversationPlan("root", "s1", listOf("root", "s1", "s3")),
                ConversationPlan("alone", "alone", listOf("alone")),
            ),
            plans,
        )
    }

    @Test
    fun `an old segment is polled only for a group, inside the retention`() {
        assertTrue(oldSegmentPolled(isHead = true, oneToOne = true, movedAtMs = 0, nowMs = Long.MAX_VALUE))
        assertFalse(oldSegmentPolled(isHead = false, oneToOne = true, movedAtMs = null, nowMs = 0))
        assertTrue(oldSegmentPolled(isHead = false, oneToOne = false, movedAtMs = null, nowMs = 0))
        assertTrue(oldSegmentPolled(false, false, 0, OLD_SEGMENT_POLL_WINDOW_MS - 1))
        assertFalse(oldSegmentPolled(false, false, 0, OLD_SEGMENT_POLL_WINDOW_MS))
    }

    @Test
    fun `an old segment's push subscription is retired once the head is live`() {
        assertFalse(retiresOldSegmentPush("g", "g", headActive = true, alreadyRetired = false))
        assertFalse(retiresOldSegmentPush("g", null, headActive = true, alreadyRetired = false))
        assertTrue(retiresOldSegmentPush("g", "h", headActive = true, alreadyRetired = false))
        assertFalse(retiresOldSegmentPush("g", "h", headActive = false, alreadyRetired = false))
        assertFalse(retiresOldSegmentPush("g", "h", headActive = true, alreadyRetired = true))
    }

    @Test
    fun `history row ids are negative, ordered and invertible`() {
        val near = historyRowId(7, 1)
        val far = historyRowId(9, 2)
        assertTrue(near < 0 && far < near)
        assertTrue(historyRowId(8, 1) > near)
        assertEquals(HistoryRowOrigin(7, 1), historyRowOrigin(near))
        assertEquals(HistoryRowOrigin(9, 2), historyRowOrigin(far))
        assertNull(historyRowOrigin(5))
        assertNull(historyRowOrigin(-HISTORY_ID_SPAN))
        assertTrue(isHistoryRowId(near))
        assertFalse(isHistoryRowId(3))
        assertFalse(isHistoryRowId(null))
    }

    @Test
    fun `pins span the segments and an older one can only be unpinned`() {
        val folded = pinsAcrossSegments(
            listOf(
                SegmentPins(0, mapOf(4L to "head"), mapOf(2L to 1)),
                SegmentPins(1, mapOf(4L to "older"), emptyMap()),
            ),
        )
        assertEquals(mapOf(4L to "head", historyRowId(4, 1) to "older"), folded.shared)
        assertEquals(mapOf(2L to 1), folded.personal)
        assertEquals(PinTarget("head", 4, false), pinTargetOfRow(4, "head", listOf("oldest", "old")))
        assertEquals(PinTarget("old", 4, true), pinTargetOfRow(historyRowId(4, 1), "head", listOf("oldest", "old")))
        assertEquals(PinTarget("oldest", 4, true), pinTargetOfRow(historyRowId(4, 2), "head", listOf("oldest", "old")))
        assertNull(pinTargetOfRow(historyRowId(4, 3), "head", listOf("oldest", "old")))
        assertEquals(PinRoute.ORDINARY, pinRoute(older = false, pinned = true, shared = true))
        assertEquals(PinRoute.ORDINARY, pinRoute(older = true, pinned = false, shared = false))
        assertEquals(PinRoute.LOCAL_ONLY, pinRoute(older = true, pinned = false, shared = true))
        assertEquals(PinRoute.REFUSED, pinRoute(older = true, pinned = true, shared = false))
        assertTrue(historyRowOffersUnpin(isHistoryRow = true, rowIsPinned = true))
        assertFalse(historyRowOffersUnpin(isHistoryRow = true, rowIsPinned = false))
        assertFalse(historyRowOffersUnpin(isHistoryRow = false, rowIsPinned = true))
    }

    @Test
    fun `the lineage mirror round-trips and drops junk`() {
        val entries = listOf(LineageEntry("ab", "cd", ULong.MAX_VALUE), LineageEntry("ef", "cd", 3UL))
        assertEquals(entries, decodeLineageMirror(encodeLineageMirror(entries)))
        assertTrue(decodeLineageMirror("not json").isEmpty())
        assertTrue(decodeLineageMirror("""[{"s":"ab","r":"cd","h":"minus"}]""").isEmpty())
    }

    @Test
    fun `the continuity facts`() {
        var row = ContinuityRow()
        row = row.withMoved("g", 10, oneToOne = true).withMoved("g", 99)
        assertEquals(mapOf("g" to 10L), row.moved)
        assertEquals(setOf("g"), row.movedOneToOne)
        row = row.withResent("g").withPushRetired("g").withAwaitedSwaps("h", mapOf("A" to 1L))
        assertEquals(row, decodeContinuityRow(encodeContinuityRow(row)))
        assertEquals(ContinuityRow(), decodeContinuityRow("garbage"))
        assertTrue(row.withAwaitedSwaps("h", emptyMap()).swapping.isEmpty())
        val store = ContinuityStore(InMemoryContinuityBackend())
        store.update("me") { it.withMoved("g", 1) }
        assertEquals(mapOf("g" to 1L), store.row("me").moved)
        assertTrue(store.row("other").moved.isEmpty())
    }
}
