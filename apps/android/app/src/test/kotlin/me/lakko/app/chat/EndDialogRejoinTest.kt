// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * UX `group-and-channel-privacy.md` 9.2 — the "You are no longer in {name}."
 * dialog of an open conversation, when its notice goes while it is up. Seen on
 * an emulator 2026-11-23: the person was added back with the dialog on screen,
 * and it stayed, now false, with `OK` leading to Chats.
 */
class EndDialogRejoinTest {

    @Test
    fun `added back - the dialog closes and the conversation stays`() {
        assertEquals(EndDialogClose.STAY, endDialogWhenNoticeGoes(held = true, endOwed = false))
    }

    @Test
    fun `the person's own leave landed - back to Chats`() {
        assertEquals(EndDialogClose.LEAVE, endDialogWhenNoticeGoes(held = false, endOwed = false))
    }

    @Test
    fun `an end still owed is not a return`() {
        assertEquals(EndDialogClose.LEAVE, endDialogWhenNoticeGoes(held = true, endOwed = true))
        assertEquals(EndDialogClose.LEAVE, endDialogWhenNoticeGoes(held = false, endOwed = true))
    }
}
