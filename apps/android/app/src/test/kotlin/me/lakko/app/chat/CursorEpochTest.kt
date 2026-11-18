// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import uniffi.lakko_ffi_uniffi.FfiException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0091 phase 1b — the epoch recorded with a conversation's cursor, and
 *  when a poll reads again from the commit leaving the loaded epoch. Same
 *  table as web `cursor-epoch.test.ts`. */
class CursorEpochTest {

    private val commits: Map<ULong, Long> = mapOf(7uL to 500L, 8uL to 640L)

    @Test
    fun `reads again from just before the commit leaving the loaded epoch`() {
        assertEquals(499L, snapshotRewindFrom(after = 700L, loaded = 6uL, recorded = 8uL, registeredCommits = commits))
        // At the cursor itself: the cursor is past it.
        assertEquals(499L, snapshotRewindFrom(after = 500L, loaded = 6uL, recorded = 7uL, registeredCommits = commits))
    }

    @Test
    fun `does not rewind otherwise`() {
        assertNull("no recorded epoch", snapshotRewindFrom(700L, 6uL, null, commits))
        assertNull("no loaded epoch", snapshotRewindFrom(700L, null, 8uL, commits))
        assertNull("at the recorded epoch", snapshotRewindFrom(700L, 8uL, 8uL, commits))
        assertNull("above the recorded epoch", snapshotRewindFrom(700L, 9uL, 8uL, commits))
        assertNull("the commit ahead of the cursor", snapshotRewindFrom(400L, 6uL, 8uL, commits))
        assertNull("no commit named for the loaded epoch", snapshotRewindFrom(700L, 4uL, 8uL, commits))
        assertNull("a read that named no commits", snapshotRewindFrom(700L, 6uL, 8uL, null))
    }

    @Test
    fun `the recorded epoch never moves backward`() {
        assertEquals(7uL, cursorEpochToSave(null, 7uL))
        assertEquals(9uL, cursorEpochToSave(7uL, 9uL))
        assertNull(cursorEpochToSave(7uL, 7uL))
        assertNull(cursorEpochToSave(7uL, 5uL))
    }

    @Test
    fun `the rewind line names epochs only`() {
        assertEquals(
            "chat snapshot behind its cursor: loaded=6 recorded=8; " +
                "reading again from the commit that leaves the loaded epoch",
            rewindLine(6uL, 8uL),
        )
    }

    @Test
    fun `an error log tag keeps the class names and the DS status, never the endpoint`() {
        val hex = "c14ae3b45bdb6a66".padEnd(64, '0')
        val ds = me.lakko.app.ds.DsHttpError(401, "/ds/group/$hex/messages", "proof required")
        val tag = errorLogTag(ChatError("retire of $hex failed", ds))
        assertEquals("ChatError>DsHttpError status=401", tag)
        assertFalse(tag.contains(hex))
        assertFalse(tag.contains("proof"))
        assertTrue(errorLogTag(FfiException.MembershipNotOwner("x")).endsWith("refusal=notOwner"))
    }
}
