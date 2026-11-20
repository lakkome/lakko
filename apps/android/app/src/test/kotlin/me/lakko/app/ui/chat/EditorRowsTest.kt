// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/** UX attachments 7.1a — the poll and checklist editors' row list. */
class EditorRowsTest {

    private fun texts(rows: EditorRows) = rows.rows.map { it.text }

    @Test
    fun `a poll starts with two empty options and a checklist with one item, no trailing row`() {
        assertEquals(listOf("", ""), texts(EditorRows.of(emptyList(), POLL_OPTIONS_MIN, POLL_OPTIONS_MAX)))
        assertEquals(listOf(""), texts(EditorRows.of(emptyList(), CHECKLIST_ITEMS_MIN, CHECKLIST_ITEMS_MAX)))
        assertEquals(listOf("a", "b"), texts(EditorRows.of(listOf("a", "b"), CHECKLIST_ITEMS_MIN, CHECKLIST_ITEMS_MAX)))
    }

    @Test
    fun `typing in the last row adds no row`() {
        val rows = EditorRows.of(emptyList(), 2, 10)
        val typed = rows.setText(rows.rows[1].id, "yes", EDITOR_ROW_MAX_CHARS)
        assertEquals(listOf("", "yes"), texts(typed))
    }

    @Test
    fun `a row is cut at the character limit`() {
        val rows = EditorRows.of(emptyList(), 1, 30)
        val typed = rows.setText(rows.rows[0].id, "x".repeat(150), EDITOR_ROW_MAX_CHARS)
        assertEquals(EDITOR_ROW_MAX_CHARS, typed.rows[0].text.length)
    }

    @Test
    fun `remove is offered only above the minimum and refused at it`() {
        val two = EditorRows.of(listOf("a", "b"), 2, 10)
        assertFalse(two.canRemove)
        assertSame(two, two.remove(two.rows[0].id))
        val three = two.add()!!.first
        assertTrue(three.canRemove)
        assertEquals(listOf("a", ""), texts(three.remove(three.rows[1].id)))
    }

    @Test
    fun `add is offered only below the maximum`() {
        var rows = EditorRows.of(emptyList(), 2, 10)
        repeat(8) { rows = rows.add()!!.first }
        assertEquals(10, rows.rows.size)
        assertFalse(rows.canAdd)
        assertNull(rows.add())
    }

    @Test
    fun `new rows get fresh ids so a removed id is never reused`() {
        val rows = EditorRows.of(listOf("a", "b", "c"), 2, 10)
        val removed = rows.remove(rows.rows[2].id)
        val (added, id) = removed.add()!!
        assertEquals(3, added.rows.size)
        assertEquals(added.rows.map { it.id }.toSet().size, added.rows.size)
        assertTrue(id !in rows.rows.map { it.id })
    }

    @Test
    fun `IME Next moves to the next row, adds one on the last, and stops at the maximum`() {
        val rows = EditorRows.of(listOf("a", "b"), 2, 3)
        val (same, nextId) = rows.next(rows.rows[0].id)!!
        assertSame(rows, same)
        assertEquals(rows.rows[1].id, nextId)
        val (grown, newId) = rows.next(rows.rows[1].id)!!
        assertEquals(3, grown.rows.size)
        assertEquals(grown.rows[2].id, newId)
        assertNull(grown.next(grown.rows[2].id))
    }

    @Test
    fun `move reorders rows and keeps their ids`() {
        val rows = EditorRows.of(listOf("a", "b", "c"), 2, 10)
        val moved = rows.move(0, 2)
        assertEquals(listOf("b", "c", "a"), texts(moved))
        assertEquals(rows.rows[0].id, moved.rows[2].id)
        assertSame(rows, rows.move(1, 1))
        assertSame(rows, rows.move(5, 0))
        assertEquals(listOf("b", "c", "a"), texts(rows.move(0, 99)))
    }

    @Test
    fun `filled keeps order and drops empty rows`() {
        val rows = EditorRows.of(listOf(" a ", "", "c"), 1, 30)
        assertEquals(listOf("a", "c"), rows.filled)
        assertTrue(rows.anyText)
        assertFalse(EditorRows.of(listOf("", " "), 1, 30).anyText)
    }

    @Test
    fun `the quiz's correct option follows its row through a move and skips empty rows`() {
        val rows = EditorRows.of(listOf("a", "", "c"), 2, 10)
        val correct = rows.rows[2].id
        assertEquals(1, pollCorrectIndex(rows, correct))
        assertEquals(0, pollCorrectIndex(rows.move(2, 0), correct))
        assertNull(pollCorrectIndex(rows, rows.rows[1].id))
        assertNull(pollCorrectIndex(rows, null))
        assertNull(pollCorrectIndex(rows.remove(correct), correct))
    }

    @Test
    fun `a checklist is ready with a title and one filled item`() {
        val empty = EditorRows.of(emptyList(), 1, 30)
        assertFalse(checklistDraftReady("Groceries", empty))
        val one = empty.setText(empty.rows[0].id, "milk", EDITOR_ROW_MAX_CHARS)
        assertFalse(checklistDraftReady(" ", one))
        assertTrue(checklistDraftReady("Groceries", one))
    }
}
