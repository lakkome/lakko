// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.ArchivedSection as SharedArchivedSection
import uniffi.lakko_ffi_uniffi.ArchivedSectionRow as SharedArchivedSectionRow
import uniffi.lakko_ffi_uniffi.ArchivedView

/**
 * `docs/ux/navigation.md` 2.2, "Archived, as built (2026-11-25)" — the WIRING
 * of the shared `archived_section` rule. The host JVM cannot load the native
 * library, so the rule is a fake here: these tests pin what the list asks it
 * (each row's archived flag and the pin of the view shown, the search, whether
 * the archive is open) and how its counts slice the rows. The rule itself is
 * tested in `crates/lakko-ui-rules` and through the FFI on iOS and web.
 */
class ArchivedListSplitTest {

    private data class Row(val id: String, val archived: Boolean, val pinned: Boolean = false)

    private class Recorder(val answer: SharedArchivedSection) {
        var rows: List<SharedArchivedSectionRow>? = null
        var searching: Boolean? = null
        var archiveOpen: Boolean? = null
        val rule: ArchivedSectionRule = { r, s, o ->
            rows = r
            searching = s
            archiveOpen = o
            answer
        }
    }

    private fun section(view: ArchivedView, top: Int, archived: Int, bar: Boolean, divider: Boolean) =
        SharedArchivedSection(view, top.toUInt(), archived.toUInt(), bar, divider)

    private val rows = listOf(
        Row("p", archived = true, pinned = true),
        Row("x", archived = false),
        Row("a", archived = true),
        Row("b", archived = true),
    )

    private fun split(rec: Recorder, searching: Boolean = false, archiveOpen: Boolean = false) =
        ArchivedListSplit.split(rows, searching, archiveOpen, { it.archived }, { it.pinned }, rec.rule)

    @Test
    fun handsTheRuleEachRowsFlagsTheSearchAndTheOpenState() {
        val rec = Recorder(section(ArchivedView.ARCHIVE, 2, 2, bar = false, divider = false))
        split(rec, searching = true, archiveOpen = true)
        assertEquals(
            listOf(
                SharedArchivedSectionRow(archived = true, pinned = true),
                SharedArchivedSectionRow(archived = false, pinned = false),
                SharedArchivedSectionRow(archived = true, pinned = false),
                SharedArchivedSectionRow(archived = true, pinned = false),
            ),
            rec.rows,
        )
        assertEquals(true, rec.searching)
        assertEquals(true, rec.archiveOpen)
    }

    @Test
    fun theChatsViewHidesTheArchivedRowsBehindTheBar() {
        val s = split(Recorder(section(ArchivedView.CHATS, 2, 2, bar = true, divider = false)))
        assertFalse(s.archiveShown)
        assertEquals(2, s.barCount)
        assertFalse(s.divider)
        assertEquals(listOf("p", "x"), s.chatRows.map { it.id })
        assertTrue(s.archivedRows.isEmpty())
    }

    @Test
    fun noBarWhenNothingIsArchived() {
        val s = split(Recorder(section(ArchivedView.CHATS, 4, 0, bar = false, divider = false)))
        assertNull(s.barCount)
        assertEquals(listOf("p", "x", "a", "b"), s.chatRows.map { it.id })
        assertTrue(s.archivedRows.isEmpty())
    }

    @Test
    fun theArchiveViewListsOnlyTheArchivedRowsInOrder() {
        val s = split(Recorder(section(ArchivedView.ARCHIVE, 2, 2, bar = false, divider = false)), archiveOpen = true)
        assertTrue(s.archiveShown)
        assertNull(s.barCount)
        assertTrue(s.chatRows.isEmpty())
        assertEquals(listOf("a", "b"), s.archivedRows.map { it.id })
    }

    @Test
    fun searchingInTheChatsViewListsArchivedMatchesUnderTheDivider() {
        val s = split(Recorder(section(ArchivedView.CHATS, 2, 2, bar = false, divider = true)), searching = true)
        assertTrue(s.divider)
        assertNull(s.barCount)
        assertEquals(listOf("p", "x"), s.chatRows.map { it.id })
        assertEquals(listOf("a", "b"), s.archivedRows.map { it.id })
    }

    @Test
    fun countsBeyondTheRowsAreClampedAndDrawNoBar() {
        val s = split(Recorder(section(ArchivedView.CHATS, 9, 9, bar = true, divider = false)))
        assertEquals(4, s.top.size)
        assertTrue(s.archived.isEmpty())
        assertNull(s.barCount)
    }
}
