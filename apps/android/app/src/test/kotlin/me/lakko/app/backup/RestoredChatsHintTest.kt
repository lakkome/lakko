// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import org.junit.Assert.assertEquals
import org.junit.Test

/** `docs/ux/identity-recovery.md` 7.2: what the chat list says after a restore. */
class RestoredChatsHintTest {

    @Test
    fun `no restore waiting means the ordinary list`() {
        assertEquals(RestoredChatsHint.NONE, restoredChatsHint(null, listEmpty = true, otherDevice = false))
    }

    @Test
    fun `an empty list with no other device gets the card when the backup named a conversation`() {
        assertEquals(RestoredChatsHint.CARD, restoredChatsHint(true, listEmpty = true, otherDevice = false))
        assertEquals(RestoredChatsHint.NONE, restoredChatsHint(false, listEmpty = true, otherDevice = false))
    }

    @Test
    fun `another device signed in gets the banner`() {
        assertEquals(RestoredChatsHint.BANNER, restoredChatsHint(true, listEmpty = true, otherDevice = true))
        assertEquals(RestoredChatsHint.BANNER, restoredChatsHint(false, listEmpty = true, otherDevice = true))
    }

    @Test
    fun `the first conversation ends both`() {
        assertEquals(RestoredChatsHint.NONE, restoredChatsHint(true, listEmpty = false, otherDevice = false))
        assertEquals(RestoredChatsHint.NONE, restoredChatsHint(true, listEmpty = false, otherDevice = true))
    }

    @Test
    fun `nothing is said until the other devices were read`() {
        assertEquals(RestoredChatsHint.NONE, restoredChatsHint(true, listEmpty = true, otherDevice = null))
    }
}
