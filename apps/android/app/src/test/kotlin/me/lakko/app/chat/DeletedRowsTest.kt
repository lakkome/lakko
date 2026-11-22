// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.ui.chat.TimelineItem
import me.lakko.app.ui.chat.buildTimeline
import me.lakko.app.ui.chat.previewSourceRow
import me.lakko.app.ui.chat.rowPreview
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant
import java.time.ZoneId
import java.util.Locale

/**
 * `docs/ux/message-actions.md` section 9 — a deleted message leaves the
 * conversation: not drawn, except in its short hold when its deletion was
 * applied in front of the person; never a list preview, never unread.
 */
class DeletedRowsTest {
    init {
        EnglishForTests.install()
    }

    private val zone: ZoneId = ZoneId.of("Europe/Berlin")
    private val now: Long = Instant.parse("2026-09-16T12:05:00Z").toEpochMilli()

    private fun msg(
        id: Long,
        deleted: Boolean = false,
        direction: Direction = Direction.Incoming,
        createdAt: String = "2026-09-16T12:00:00Z",
        text: String = "m$id",
    ) = ChatMessage(
        messageId = id,
        direction = direction,
        text = if (deleted) "" else text,
        createdAt = createdAt,
        authorKey = "anna",
        deleted = deleted,
    )

    @Test
    fun `the durations are the spec's`() {
        assertEquals(2_000L, DeletedRows.HOLD_MS)
        assertEquals(250L, DeletedRows.COLLAPSE_MS)
    }

    // --- which rows enter the hold ------------------------------------------

    @Test
    fun `a row drawn before its deletion enters the hold when the conversation is in front`() {
        val hold = DeletedRowHold()
        assertTrue(hold.observe(listOf(msg(1), msg(2)), inFront = true).isEmpty())
        val entered = hold.observe(listOf(msg(1), msg(2, deleted = true)), inFront = true)
        assertEquals(setOf(2L), entered)
        assertEquals(mapOf(2L to DeletedRowPhase.HOLD), hold.phases)
    }

    @Test
    fun `a deletion applied while the conversation is not in front is never held`() {
        val hold = DeletedRowHold()
        hold.observe(listOf(msg(1), msg(2)), inFront = true)
        assertTrue(hold.observe(listOf(msg(1), msg(2, deleted = true)), inFront = false).isEmpty())
        // Back in front: the row is simply not there, nothing is held.
        assertTrue(hold.observe(listOf(msg(1), msg(2, deleted = true)), inFront = true).isEmpty())
        assertTrue(hold.phases.isEmpty())
    }

    @Test
    fun `a message and its deletion in one batch were never drawn and are never held`() {
        val hold = DeletedRowHold()
        hold.observe(listOf(msg(1)), inFront = true)
        assertTrue(hold.observe(listOf(msg(1), msg(2, deleted = true)), inFront = true).isEmpty())
    }

    @Test
    fun `the first list a fresh screen sees holds nothing (a restart, a restored history)`() {
        val hold = DeletedRowHold()
        assertTrue(hold.observe(listOf(msg(1, deleted = true), msg(2)), inFront = true).isEmpty())
    }

    @Test
    fun `each deleted row of a bulk delete enters the hold`() {
        val hold = DeletedRowHold()
        hold.observe(listOf(msg(1), msg(2), msg(3)), inFront = true)
        val entered = hold.observe(listOf(msg(1, deleted = true), msg(2), msg(3, deleted = true)), inFront = true)
        assertEquals(setOf(1L, 3L), entered)
    }

    @Test
    fun `a held row is not entered twice, collapses, then is released`() {
        val hold = DeletedRowHold()
        hold.observe(listOf(msg(1)), inFront = true)
        hold.observe(listOf(msg(1, deleted = true)), inFront = true)
        assertTrue(hold.observe(listOf(msg(1, deleted = true)), inFront = true).isEmpty())
        assertTrue(hold.startCollapse(1))
        assertEquals(DeletedRowPhase.COLLAPSE, hold.phases[1])
        assertTrue(hold.release(1))
        assertTrue(hold.phases.isEmpty())
        assertFalse(hold.startCollapse(1))
    }

    @Test
    fun `leaving the front ends every hold at once`() {
        val hold = DeletedRowHold()
        hold.observe(listOf(msg(1)), inFront = true)
        hold.observe(listOf(msg(1, deleted = true)), inFront = true)
        hold.leaveFront()
        assertTrue(hold.phases.isEmpty())
        assertFalse(hold.startCollapse(1))
    }

    @Test
    fun `an id-based timeline (a channel) holds a post that was drawn and is now deleted`() {
        val hold = DeletedRowHold()
        hold.observeIds(drawn = setOf(10L, 11L), deleted = emptySet(), present = setOf(10L, 11L), inFront = true)
        val entered = hold.observeIds(drawn = setOf(10L), deleted = setOf(11L), present = setOf(10L, 11L), inFront = true)
        assertEquals(setOf(11L), entered)
    }

    // --- what is drawn --------------------------------------------------------

    @Test
    fun `the timeline draws no deleted message except a held one`() {
        val messages = listOf(msg(1), msg(2, deleted = true), msg(3, deleted = true))
        val ids = buildTimeline(messages, now, zone, use24h = true, locale = Locale.UK, heldDeleted = setOf(3L))
            .filterIsInstance<TimelineItem.Row>()
            .map { it.message.messageId }
        assertEquals(listOf(1L, 3L), ids)
    }

    @Test
    fun `a day with nothing left under it draws no separator`() {
        val messages = listOf(
            msg(1, deleted = true, createdAt = "2026-09-14T09:00:00Z"),
            msg(2, createdAt = "2026-09-16T09:00:00Z"),
        )
        val items = buildTimeline(messages, now, zone, use24h = true, locale = Locale.UK)
        assertEquals(1, items.count { it is TimelineItem.DateSeparator })
        assertEquals(listOf(2L), items.filterIsInstance<TimelineItem.Row>().map { it.message.messageId })
    }

    @Test
    fun `runs are computed over the messages that remain`() {
        // 1 and 3 from Anna with 2 (Anna's, deleted) between: one run.
        val messages = listOf(msg(1), msg(2, deleted = true), msg(3))
        val rows = buildTimeline(messages, now, zone, use24h = true, locale = Locale.UK)
            .filterIsInstance<TimelineItem.Row>()
        assertTrue(rows[0].firstInRun)
        assertFalse(rows[1].firstInRun)
    }

    // --- the chat list ----------------------------------------------------------

    @Test
    fun `the list preview is the newest message that is not deleted`() {
        val messages = listOf(
            msg(1, text = "hello", createdAt = "2026-09-16T10:00:00Z"),
            msg(2, deleted = true, createdAt = "2026-09-16T11:00:00Z"),
        )
        assertEquals(1L, DeletedRows.previewSource(messages)?.messageId)
        assertEquals(1L, previewSourceRow(messages)?.messageId)
    }

    @Test
    fun `with every message deleted the list reads as a conversation with no messages`() {
        val messages = listOf(msg(1, deleted = true), msg(2, deleted = true))
        assertNull(previewSourceRow(messages))
        val thread = ChatThread(groupIdHex = "ab".repeat(32), messages = messages, cursor = 0L)
        assertEquals("No messages yet", rowPreview(thread, previewSourceRow(messages), isGroup = false, senderName = null))
    }

    // --- unread -------------------------------------------------------------------

    @Test
    fun `a deleted message leaves the local unread count`() {
        val counter = LocalUnreadCounter()
        counter.onIncomingApplied("g", listOf(1L, 2L, 3L))
        assertEquals(3, counter.snapshot("g").count)
        assertEquals(2, counter.snapshot("g", deleted = setOf(2L)).count)
        assertEquals(0, counter.snapshot("g", deleted = setOf(1L, 2L, 3L)).count)
    }

    @Test
    fun `a deleted message counts as seen for the feed's contiguous rule`() {
        val rows = listOf(
            me.lakko.app.feed.SeenRow(1, "incoming"),
            me.lakko.app.feed.SeenRow(2, "incoming", deleted = true),
            me.lakko.app.feed.SeenRow(3, "incoming"),
        )
        val result = me.lakko.app.feed.markSeen(
            rows = rows,
            state = me.lakko.app.feed.GroupSeenState(through = null, feedSeen = emptySet()),
            ids = listOf(1L, 3L),
            contiguous = { ids, seen ->
                var through: Long? = null
                for (id in ids) if (id in seen) through = id else break
                through
            },
        )
        assertEquals(3L, result.advancedTo)
    }
}
