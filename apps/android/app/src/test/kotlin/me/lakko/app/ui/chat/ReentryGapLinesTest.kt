// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.Direction
import me.lakko.app.chat.ReentryGap
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0091 phase 2 / `docs/ux/conversation-rejoin.md` 2.4 — where the gap
 * line goes in the timeline.
 */
class ReentryGapLinesTest {

    private fun row(id: Long, createdAt: String) = TimelineItem.Row(
        message = ChatMessage(messageId = id, direction = Direction.Incoming, text = "m$id", createdAt = createdAt),
        firstInRun = true,
        lastInRun = true,
        time = null,
    )

    private val items = listOf(
        row(1L, "2026-11-11T12:00:00Z"),
        row(2L, "2026-11-11T13:00:00Z"),
        row(3L, "2026-11-18T15:00:00Z"),
    )

    private fun keys(list: List<TimelineItem>) = list.map { it.key }

    @Test
    fun `no gaps leaves the timeline as it is`() {
        assertSame(items, withReentryGapLines(items, emptyList()) { "x" })
    }

    private fun ms(iso: String) = java.time.Instant.parse(iso).toEpochMilli()

    @Test
    fun `the line sits after the last message at or before the gap's start`() {
        val fromMs = ms("2026-11-11T13:30:00Z")
        val out = withReentryGapLines(items, listOf(ReentryGap(fromMs, ms("2026-11-18T16:00:00Z")))) { "gap" }
        assertEquals(
            listOf("m-${items[0].message.rowId}", "m-${items[1].message.rowId}", "gap-$fromMs-${ms("2026-11-18T16:00:00Z")}", "m-${items[2].message.rowId}"),
            keys(out),
        )
        assertEquals("gap", (out[2] as TimelineItem.DateSeparator).label)
    }

    @Test
    fun `a message sent after the replacement but before the local join stays below the line`() {
        // Walk 2, cycle 2: the old rule (before the first message from the
        // gap's END on) put this message above the line.
        val fromMs = ms("2026-11-11T13:30:00Z")
        val toMs = ms("2026-11-18T16:00:00Z")
        val out = withReentryGapLines(items, listOf(ReentryGap(fromMs, toMs, anchorMessageId = 3L))) { "gap" }
        assertEquals("gap-$fromMs-$toMs", out[2].key)
        assertEquals("m-${items[2].message.rowId}", out[3].key)
    }

    @Test
    fun `a message exactly at the gap's start stays above the line`() {
        val fromMs = ms("2026-11-11T13:00:00Z")
        val out = withReentryGapLines(items, listOf(ReentryGap(fromMs, fromMs + 1))) { "gap" }
        assertEquals("gap-$fromMs-${fromMs + 1}", out[2].key)
    }

    @Test
    fun `when every message is later the line goes first, above the first day's separator`() {
        val sep = TimelineItem.DateSeparator(key = "d-1", label = "11 Nov")
        val withDay = listOf(sep) + items
        val out = withReentryGapLines(withDay, listOf(ReentryGap(10L, 20L))) { "gap" }
        assertEquals("gap-10-20", out[0].key)
        assertEquals("d-1", out[1].key)
    }

    @Test
    fun `two gaps at one place keep their order`() {
        val out = withReentryGapLines(
            items,
            listOf(ReentryGap(ms("2026-11-11T13:40:00Z"), 2L + ms("2026-11-11T13:40:00Z")), ReentryGap(ms("2026-11-11T13:10:00Z"), ms("2026-11-11T13:20:00Z"))),
        ) { "gap" }
        assertEquals("gap-${ms("2026-11-11T13:10:00Z")}-${ms("2026-11-11T13:20:00Z")}", out[2].key)
        assertEquals("gap-${ms("2026-11-11T13:40:00Z")}-${2L + ms("2026-11-11T13:40:00Z")}", out[3].key)
    }

    @Test
    fun `with nothing after its start the line goes last`() {
        val out = withReentryGapLines(items, listOf(ReentryGap(Long.MAX_VALUE - 2, Long.MAX_VALUE - 1))) { "gap" }
        assertEquals("gap-${Long.MAX_VALUE - 2}-${Long.MAX_VALUE - 1}", out.last().key)
    }

    @Test
    fun `a chat whose every message was missed still draws its gap line`() {
        val timeline = withReentryGapLines(emptyList(), listOf(ReentryGap(10L, 20L))) { "gap" }
        assertEquals(listOf("gap-10-20"), keys(timeline))
        assertTrue(timeline.single() is TimelineItem.DateSeparator)
        assertFalse(showsEmptyTimeline(timeline, messagesEmpty = true, pendingMediaEmpty = true))
    }

    @Test
    fun `an empty chat with no gap line shows the empty hint`() {
        assertTrue(showsEmptyTimeline(emptyList(), messagesEmpty = true, pendingMediaEmpty = true))
        assertFalse("a send in flight draws the list", showsEmptyTimeline(emptyList(), messagesEmpty = true, pendingMediaEmpty = false))
        assertFalse(showsEmptyTimeline(items, messagesEmpty = false, pendingMediaEmpty = true))
    }
}
