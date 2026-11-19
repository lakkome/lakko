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

    @Test
    fun `the line follows its anchor message`() {
        val out = withReentryGapLines(items, listOf(ReentryGap(10L, 20L, anchorMessageId = 2L))) { "gap" }
        assertEquals(listOf("m-${items[0].message.rowId}", "m-${items[1].message.rowId}", "gap-10-20", "m-${items[2].message.rowId}"), keys(out))
        assertEquals("gap", (out[2] as TimelineItem.DateSeparator).label)
    }

    @Test
    fun `without its anchor the line goes before the first message from the gap's end on`() {
        val toMs = java.time.Instant.parse("2026-11-18T14:02:00Z").toEpochMilli()
        val out = withReentryGapLines(items, listOf(ReentryGap(10L, toMs, anchorMessageId = 99L))) { "gap" }
        assertEquals("gap-10-$toMs", out[2].key)
    }

    @Test
    fun `with nothing after it the line goes last`() {
        val out = withReentryGapLines(items, listOf(ReentryGap(10L, Long.MAX_VALUE - 1))) { "gap" }
        assertEquals("gap-10-${Long.MAX_VALUE - 1}", out.last().key)
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
