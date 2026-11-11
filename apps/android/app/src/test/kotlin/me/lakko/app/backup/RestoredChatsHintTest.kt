// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * `docs/ux/identity-recovery.md` 7.2 (amended 2026-11-11): what the chat list
 * says after a restore. The card needs this device's DeviceGroup rejoin to have
 * founded (nobody answered its knock), never a directory listing; there is no
 * banner.
 */
class RestoredChatsHintTest {

    private val answered = RestoreRejoinAnswer.ANSWERED
    private val founded = RestoreRejoinAnswer.FOUNDED

    @Test
    fun `no restore waiting means the ordinary list`() {
        for (rejoin in listOf(null, answered, founded)) {
            assertEquals(RestoredChatsHint.NONE, restoredChatsHint(null, listEmpty = true, rejoin = rejoin))
        }
    }

    @Test
    fun `another device answered the knock shows the ordinary list`() {
        assertEquals(RestoredChatsHint.NONE, restoredChatsHint(true, listEmpty = true, rejoin = answered))
    }

    @Test
    fun `a device that founded because nobody answered gets the card`() {
        assertEquals(RestoredChatsHint.CARD, restoredChatsHint(true, listEmpty = true, rejoin = founded))
    }

    @Test
    fun `a backup that named no conversation shows the ordinary empty list`() {
        assertEquals(RestoredChatsHint.NONE, restoredChatsHint(false, listEmpty = true, rejoin = answered))
        assertEquals(RestoredChatsHint.NONE, restoredChatsHint(false, listEmpty = true, rejoin = founded))
    }

    @Test
    fun `the first conversation ends both`() {
        assertEquals(RestoredChatsHint.NONE, restoredChatsHint(true, listEmpty = false, rejoin = answered))
        assertEquals(RestoredChatsHint.NONE, restoredChatsHint(true, listEmpty = false, rejoin = founded))
    }

    @Test
    fun `nothing is said until the rejoin decided`() {
        assertEquals(RestoredChatsHint.NONE, restoredChatsHint(true, listEmpty = true, rejoin = null))
    }
}
