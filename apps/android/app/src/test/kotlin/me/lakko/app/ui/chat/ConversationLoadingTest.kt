// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.chat.showsSettingUpLine
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/navigation.md` section 2.4 — an open conversation draws nothing of
 * itself before this device's conversations were read: no "No messages yet",
 * no "being set up" line.
 */
class ConversationLoadingTest {

    @Test
    fun anEmptyTimelineIsNotSaidToBeEmptyBeforeTheMessagesWereRead() {
        assertFalse(
            showsEmptyTimeline(emptyList(), messagesEmpty = true, pendingMediaEmpty = true, messagesKnown = false),
        )
        assertTrue(
            showsEmptyTimeline(emptyList(), messagesEmpty = true, pendingMediaEmpty = true, messagesKnown = true),
        )
    }

    @Test
    fun theSettingUpLineWaitsForTheConversationsToBeRead() {
        assertFalse(showsSettingUpLine(threadKnown = false, ending = false, loaded = false))
        assertTrue(showsSettingUpLine(threadKnown = false, ending = false, loaded = true))
        assertFalse(showsSettingUpLine(threadKnown = true, ending = false, loaded = true))
    }
}
