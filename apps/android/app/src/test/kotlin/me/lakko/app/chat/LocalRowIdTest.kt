// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test

/**
 * A row this device writes locally (a call, a system line) must not take the identity of
 * a row already there. The timeline keys on `rowId`, and a repeated key brings the screen
 * down: ending a call in a chat where this device had sent a message did exactly that.
 */
class LocalRowIdTest {
    private fun row(messageId: Long, localId: Long = 0L) = ChatMessage(
        messageId = messageId,
        direction = Direction.Outgoing,
        text = "x",
        createdAt = "2026-10-31T23:00:00Z",
        localId = localId,
    )

    @Test
    fun anEmptyConversationStartsBelowZero() {
        assertEquals(-1L, nextLocalRowId(emptyList()))
    }

    @Test
    fun aSentRowKeepsItsLocalIdSoTheNextRowGoesBelowIt() {
        val sent = row(messageId = 7L, localId = -1L)
        val next = nextLocalRowId(listOf(sent))
        assertNotEquals(sent.rowId, next)
        assertEquals(-2L, next)
    }

    @Test
    fun aRowOfMessageIdOnlyGoesBelowZero() {
        assertEquals(-1L, nextLocalRowId(listOf(row(messageId = 7L))))
    }
}
