// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * The DeviceGroup receive loop's persisted cursor: a restart resumes past this
 * device's own rows instead of re-reading the journal, and the stored value
 * never leads the durable snapshot (C-67).
 */
class DeviceGroupJournalCursorTest {

    private val groupA = ByteArray(32) { 0x11 }
    private val groupB = ByteArray(32) { 0x22 }

    @Test
    fun a_stored_cursor_round_trips_for_its_journal() {
        val stored = DeviceGroupJournalCursor.encode(groupA, 276L)
        assertEquals(276L, DeviceGroupJournalCursor.decode(stored, groupA))
        assertEquals(Long.MAX_VALUE, DeviceGroupJournalCursor.decode(DeviceGroupJournalCursor.encode(groupA, Long.MAX_VALUE), groupA))
    }

    @Test
    fun a_cursor_of_another_journal_reads_as_zero() {
        // A key reset founds a new DeviceGroup journal; the old position must
        // never skip its rows.
        val stored = DeviceGroupJournalCursor.encode(groupA, 500L)
        assertEquals(0L, DeviceGroupJournalCursor.decode(stored, groupB))
    }

    @Test
    fun absent_or_damaged_bytes_read_as_zero() {
        assertEquals(0L, DeviceGroupJournalCursor.decode(null, groupA))
        assertEquals(0L, DeviceGroupJournalCursor.decode(ByteArray(5), groupA))
        assertEquals(0L, DeviceGroupJournalCursor.decode(DeviceGroupJournalCursor.encode(groupA, -4L), groupA))
    }

    @Test
    fun an_own_row_always_passes() {
        assertEquals(FailedRowVerdict.PASS, failedRowVerdict(true, 7u, 8u, 7u))
        assertEquals(FailedRowVerdict.PASS, failedRowVerdict(true, null, null, null))
    }

    @Test
    fun a_foreign_commit_at_or_above_the_handle_epoch_holds() {
        assertEquals(FailedRowVerdict.HOLD, failedRowVerdict(false, 7u, 8u, 7u))
        assertEquals(FailedRowVerdict.HOLD, failedRowVerdict(false, 9u, 10u, 7u))
        assertEquals("an unreadable handle epoch holds", FailedRowVerdict.HOLD, failedRowVerdict(false, 7u, 8u, null))
    }

    @Test
    fun rows_that_can_never_apply_pass() {
        assertEquals("an older commit", FailedRowVerdict.PASS, failedRowVerdict(false, 6u, 7u, 7u))
        assertEquals("an application message", FailedRowVerdict.PASS, failedRowVerdict(false, 7u, 7u, 7u))
        assertEquals("not MLS framing", FailedRowVerdict.PASS, failedRowVerdict(false, null, null, 7u))
    }

    @Test
    fun a_batch_of_own_echoes_moves_the_stored_cursor_past_them() {
        val tracker = JournalCursorTracker(0L)
        for (id in 1L..229L) tracker.refused(id, failedRowVerdict(true, 3u, 3u, 4u))
        assertEquals(229L, tracker.cursorToWrite())
        tracker.cursorWritten(229L)
        assertNull("nothing new to store", tracker.cursorToWrite())
    }

    @Test
    fun the_stored_cursor_stops_before_a_held_commit_for_the_rest_of_the_run() {
        val tracker = JournalCursorTracker(10L)
        tracker.passed(11L)
        tracker.refused(12L, FailedRowVerdict.HOLD)
        tracker.passed(13L)
        tracker.snapshotPersisted()
        assertEquals(11L, tracker.cursorToWrite())
        tracker.cursorWritten(11L)
        tracker.passed(20L)
        assertNull(tracker.cursorToWrite())
    }

    @Test
    fun a_failed_snapshot_write_keeps_the_cursor_until_a_later_write_succeeds() {
        val tracker = JournalCursorTracker(0L)
        tracker.passed(5L)
        tracker.snapshotPersistFailed()
        assertNull("rows applied in memory only", tracker.cursorToWrite())
        // A later batch of own echoes only: still nothing on disk covers row 5.
        tracker.refused(6L, FailedRowVerdict.PASS)
        assertNull(tracker.cursorToWrite())
        tracker.passed(7L)
        tracker.snapshotPersisted()
        assertEquals(7L, tracker.cursorToWrite())
    }

    @Test
    fun the_cursor_never_moves_backward() {
        val tracker = JournalCursorTracker(50L)
        tracker.passed(40L)
        assertNull(tracker.cursorToWrite())
    }
}
