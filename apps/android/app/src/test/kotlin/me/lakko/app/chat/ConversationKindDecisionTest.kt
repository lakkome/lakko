// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * docs/ux/client-redesign.md section 3 — the persisted conversation kind.
 *
 * A conversation is derived once from its people count when first seen, may
 * flip chat -> group when a third person is observed, and never flips back.
 */
class ConversationKindDecisionTest {

    @Test
    fun `first sight with two people is a chat and is persisted`() {
        val d = decideConversationKind(stored = null, peopleCount = 2)
        assertEquals(ConversationKind.Chat, d.kind)
        assertTrue(d.persist)
    }

    @Test
    fun `first sight with one person is still a chat`() {
        val d = decideConversationKind(stored = null, peopleCount = 1)
        assertEquals(ConversationKind.Chat, d.kind)
        assertTrue(d.persist)
    }

    @Test
    fun `first sight with three people is a group and is persisted`() {
        val d = decideConversationKind(stored = null, peopleCount = 3)
        assertEquals(ConversationKind.Group, d.kind)
        assertTrue(d.persist)
    }

    @Test
    fun `first sight without a readable count shows a chat and persists nothing`() {
        val d = decideConversationKind(stored = null, peopleCount = null)
        assertEquals(ConversationKind.Chat, d.kind)
        assertFalse(d.persist)
    }

    @Test
    fun `a stored chat stands while two people are present`() {
        val d = decideConversationKind(stored = ConversationKind.Chat, peopleCount = 2)
        assertEquals(ConversationKind.Chat, d.kind)
        assertFalse(d.persist)
    }

    @Test
    fun `a stored chat stands while the count is unreadable`() {
        val d = decideConversationKind(stored = ConversationKind.Chat, peopleCount = null)
        assertEquals(ConversationKind.Chat, d.kind)
        assertFalse(d.persist)
    }

    @Test
    fun `a stored chat flips to a group once a third person is observed`() {
        val d = decideConversationKind(stored = ConversationKind.Chat, peopleCount = 3)
        assertEquals(ConversationKind.Group, d.kind)
        assertTrue(d.persist)
    }

    @Test
    fun `a stored group never flips back, whatever the count`() {
        for (count in listOf(null, 0, 1, 2, 3, 40)) {
            val d = decideConversationKind(stored = ConversationKind.Group, peopleCount = count)
            assertEquals("count=$count", ConversationKind.Group, d.kind)
            assertFalse("count=$count", d.persist)
        }
    }

    @Test
    fun `an explicit group write is always allowed`() {
        assertTrue(kindWriteAllowed(stored = null, requested = ConversationKind.Group))
        assertTrue(kindWriteAllowed(stored = ConversationKind.Chat, requested = ConversationKind.Group))
        assertTrue(kindWriteAllowed(stored = ConversationKind.Group, requested = ConversationKind.Group))
    }

    @Test
    fun `an explicit chat write is allowed except over a stored group`() {
        assertTrue(kindWriteAllowed(stored = null, requested = ConversationKind.Chat))
        assertTrue(kindWriteAllowed(stored = ConversationKind.Chat, requested = ConversationKind.Chat))
        assertFalse(kindWriteAllowed(stored = ConversationKind.Group, requested = ConversationKind.Chat))
    }
}
