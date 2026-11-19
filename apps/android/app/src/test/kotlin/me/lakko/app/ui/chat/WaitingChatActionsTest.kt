// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0091 phase 2 / `docs/ux/conversation-rejoin.md` 2.2 — a waiting chat's
 * message menu offers Copy, Forward, Save and Pin for me, nothing else.
 */
class WaitingChatActionsTest {
    init {
        EnglishForTests.install()
    }

    private val full = messageActionsFor(
        isOwn = true,
        deleted = false,
        text = "hello",
        menuAvailable = true,
    ).copy(
        canTranslate = true,
        canBan = true,
        contentRows = listOf(ContentMenuRow("Close poll") {}),
    )

    @Test
    fun `an own message in a waiting chat keeps Copy, Forward, Save and Pin only`() {
        assertTrue("precondition: the ordinary menu has everything", full.canEdit && full.canDelete && full.canReply)
        val waiting = waitingChatActions(full)
        assertTrue(waiting.canCopy)
        assertTrue(waiting.canForward)
        assertTrue(waiting.canSave)
        assertTrue(waiting.canPin)
        assertFalse(waiting.canEdit)
        assertFalse(waiting.canDelete)
        assertFalse(waiting.canReply)
        assertFalse(waiting.canSelect)
        assertFalse(waiting.canTranslate)
        assertFalse(waiting.canBan)
        assertEquals(emptyList<ContentMenuRow>(), waiting.contentRows)
    }

    @Test
    fun `what the menu did not offer it still does not offer`() {
        val deleted = messageActionsFor(isOwn = false, deleted = true, text = "", menuAvailable = false)
        assertEquals(deleted.copy(canTranslate = false, canBan = false), waitingChatActions(deleted))
        assertFalse(waitingChatActions(deleted).any)
    }
}
