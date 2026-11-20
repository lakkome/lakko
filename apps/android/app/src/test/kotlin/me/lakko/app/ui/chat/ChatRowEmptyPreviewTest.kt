// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.ChatThread
import me.lakko.app.chat.Direction
import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The second line of a chat-list row with no message.
 *
 * ## The defect behind this
 *
 * The repository publishes its threads before it seeds them from the message
 * log, so on a cold start every row read `No messages yet` until the previews
 * arrived. `No messages yet` is a claim, made only once the log was read.
 */
class ChatRowEmptyPreviewTest {

    private val strings = EnglishForTests.strings

    private fun thread(messages: List<ChatMessage>) =
        ChatThread(groupIdHex = "cc".repeat(16), messages = messages, cursor = 0)

    @Test
    fun an_empty_thread_says_so_once_the_log_was_read() {
        assertEquals(
            "No messages yet",
            rowPreview(thread(emptyList()), null, isGroup = false, senderName = null, strings = strings),
        )
    }

    @Test
    fun an_empty_thread_shows_nothing_while_the_log_is_read() {
        assertEquals(
            "",
            rowPreview(
                thread(emptyList()),
                null,
                isGroup = false,
                senderName = null,
                strings = strings,
                messagesKnown = false,
            ),
        )
    }

    @Test
    fun a_known_message_shows_whatever_the_flag() {
        val hi = ChatMessage(1, Direction.Incoming, "hi", createdAt = "t")
        assertEquals(
            "hi",
            rowPreview(thread(listOf(hi)), hi, isGroup = false, senderName = null, strings = strings, messagesKnown = false),
        )
    }
}
