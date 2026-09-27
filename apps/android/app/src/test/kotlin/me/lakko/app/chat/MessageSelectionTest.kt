// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/message-actions.md` section 8 — the pure rules of Select: who can
 * be selected, the 100 cap, the copy, the gates, and the in-order runner that
 * both bulk actions share.
 */
class MessageSelectionTest {
    private fun message(
        id: Long,
        direction: Direction = Direction.Incoming,
        deleted: Boolean = false,
        ack: Ack = Ack.SENT,
    ) = ChatMessage(
        messageId = id,
        direction = direction,
        text = "m$id",
        createdAt = "1970-01-01T00:00:00Z",
        authorKey = "Ym9i",
        deleted = deleted,
        ack = ack,
    )

    // ----- entering and toggling -------------------------------------------

    @Test
    fun `entering selects the message the card was opened on`() {
        val s = MessageSelection.starting(7L, isOwn = true)
        assertTrue(7L in s)
        assertEquals(1, s.count)
        assertEquals(setOf(7L), s.own)
    }

    @Test
    fun `a toggle selects then deselects, keeping own and others apart`() {
        var s = MessageSelection()
        s = s.toggle(1L, isOwn = true).toggle(2L, isOwn = false)
        assertEquals(setOf(1L), s.own)
        assertEquals(setOf(2L), s.others)
        s = s.toggle(1L, isOwn = true)
        assertFalse(1L in s)
        assertEquals(setOf(2L), s.ids)
        s = s.toggle(2L, isOwn = false)
        assertTrue(s.isEmpty)
    }

    @Test
    fun `the hundred-and-first tap does nothing, deselecting still works`() {
        var s = MessageSelection()
        for (id in 1L..MESSAGE_SELECTION_LIMIT) s = s.toggle(id, isOwn = id % 2 == 0L)
        assertTrue(s.atLimit)
        assertEquals(100, s.count)
        val unchanged = s.toggle(500L, isOwn = true)
        assertSame(s, unchanged)
        assertFalse(500L in unchanged)
        // A row already selected can still be taken out at the limit.
        val fewer = s.toggle(1L, isOwn = false)
        assertEquals(99, fewer.count)
        assertFalse(fewer.atLimit)
    }

    @Test
    fun `retain keeps only the unsent ids`() {
        val s = MessageSelection(own = setOf(1L, 2L, 3L), others = setOf(4L))
        val kept = s.retain(listOf(2L, 3L, 4L, 99L))
        assertEquals(setOf(2L, 3L), kept.own)
        assertEquals(setOf(4L), kept.others)
    }

    // ----- copy ------------------------------------------------------------

    @Test
    fun `the count label reads N selected and names the limit at the cap`() {
        assertEquals("1 selected", MessageSelection.starting(1L, true).countLabel)
        var s = MessageSelection()
        for (id in 1L..12L) s = s.toggle(id, isOwn = true)
        assertEquals("12 selected", s.countLabel)
        for (id in 13L..100L) s = s.toggle(id, isOwn = true)
        assertEquals("100 selected, the limit", s.countLabel)
    }

    @Test
    fun `the confirmation wording is singular for one`() {
        assertEquals("Delete 1 message?", confirmDeleteLabel(1))
        assertEquals("Delete 12 messages?", confirmDeleteLabel(12))
    }

    @Test
    fun `the progress copy names the action and the counts`() {
        assertEquals("Deleted 5 of 12.", progressLabel(BulkAction.DELETE, 5, 12))
        assertEquals("Forwarded 5 of 12.", progressLabel(BulkAction.FORWARD, 5, 12))
        assertEquals("Deleted 0 of 1.", progressLabel(BulkAction.DELETE, 0, 1))
    }

    @Test
    fun `the mark is named by the message and states its selection`() {
        assertEquals("hello there, Selected", selectionMarkLabel("hello there", selected = true))
        assertEquals("Photo, Not selected", selectionMarkLabel("Photo", selected = false))
    }

    // ----- gates -----------------------------------------------------------

    @Test
    fun `delete is enabled only for a non-empty selection of own messages`() {
        val empty = MessageSelection()
        assertFalse(empty.deleteEnabled)
        assertFalse(empty.forwardEnabled)
        assertNull(empty.deleteHelper)

        val own = MessageSelection.starting(1L, isOwn = true)
        assertTrue(own.deleteEnabled)
        assertTrue(own.forwardEnabled)
        assertNull(own.deleteHelper)

        val mixed = own.toggle(2L, isOwn = false)
        assertFalse(mixed.deleteEnabled)
        assertTrue(mixed.forwardEnabled)
        assertEquals(ONLY_OWN_DELETABLE_HELPER, mixed.deleteHelper)

        // Taking the foreign message back out re-enables Delete.
        assertTrue(mixed.toggle(2L, isOwn = false).deleteEnabled)
    }

    @Test
    fun `only a sent, undeleted, non-system row with an id can be selected`() {
        assertTrue(canSelectMessage(message(1L)))
        assertTrue(canSelectMessage(message(2L, direction = Direction.Outgoing)))
        assertFalse(canSelectMessage(message(3L, direction = Direction.System)))
        assertFalse(canSelectMessage(message(4L, deleted = true)))
        assertFalse(canSelectMessage(message(5L, direction = Direction.Outgoing, ack = Ack.PENDING)))
        assertFalse(canSelectMessage(message(6L, direction = Direction.Outgoing, ack = Ack.FAILED)))
        assertFalse(canSelectMessage(message(0L, direction = Direction.Outgoing)))
    }

    // ----- order -----------------------------------------------------------

    @Test
    fun `the actions run in the timeline's order, oldest first, whatever the tap order`() {
        val timeline = listOf(message(10L), message(11L), message(12L), message(13L))
        val s = MessageSelection().toggle(13L, false).toggle(10L, false).toggle(12L, false)
        val ordered = selectedInTimelineOrder(s, timeline)
        assertEquals(listOf(10L, 12L, 13L), ordered.map { it.messageId })
    }

    @Test
    fun `a selected id the timeline no longer holds is skipped`() {
        val s = MessageSelection().toggle(10L, false).toggle(77L, false)
        val ordered = selectedInTimelineOrder(s, listOf(message(10L)))
        assertEquals(listOf(10L), ordered.map { it.messageId })
    }

    // ----- the runner ------------------------------------------------------

    @Test
    fun `the runner acts on every item in order and reports completion`() = runTest {
        val seen = mutableListOf<Int>()
        val result = performInOrder(listOf(1, 2, 3)) { seen += it }
        assertEquals(listOf(1, 2, 3), seen)
        assertTrue(result.complete)
        assertEquals(3, result.done)
        assertEquals(3, result.total)
        assertTrue(result.unsent.isEmpty())
        assertNull(result.failure)
    }

    @Test
    fun `the runner stops at the first failure and names what was not sent`() = runTest {
        val seen = mutableListOf<Int>()
        val result = performInOrder(listOf(1, 2, 3, 4, 5)) {
            seen += it
            if (it == 3) throw IllegalStateException("no network")
        }
        // Nothing after the failed item is attempted.
        assertEquals(listOf(1, 2, 3), seen)
        assertFalse(result.complete)
        assertEquals(2, result.done)
        assertEquals(5, result.total)
        // The failed item is unsent too: it did not land.
        assertEquals(listOf(3, 4, 5), result.unsent)
        assertNotNull(result.failure)
        assertEquals("Deleted 2 of 5.", progressLabel(BulkAction.DELETE, result.done, result.total))
    }

    @Test
    fun `an empty run is complete`() = runTest {
        val result = performInOrder(emptyList<Int>()) { }
        assertTrue(result.complete)
        assertEquals(0, result.total)
    }
}
