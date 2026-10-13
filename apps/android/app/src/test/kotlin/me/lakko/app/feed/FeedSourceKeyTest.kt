// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** ADR-0073 R.5 - a feed source is keyed for the person's stores on the
 *  conversation id (mirror of the web `feedSourceKey` / `groupFeedSources`). */
class FeedSourceKeyTest {
    private val moved = GroupConversationForFeed(conversationHex = "root", headHex = "head", title = "T", audience = 3)
    private val still = GroupConversationForFeed(conversationHex = "solo", headHex = "solo", title = "S", audience = 2)

    @Test
    fun `one source per conversation drawn from its current group`() {
        val sources = groupFeedSources(listOf(moved, still, moved.copy(headHex = "other"))) { it == "root" }
        assertEquals(listOf("head", "solo"), sources.map { it.idHex })
        assertEquals("root", sources[0].conversationHex)
        assertNull(sources[1].conversationHex)
        // The mute flag is the conversation's.
        assertEquals(listOf(true, false), sources.map { it.muted })
        assertEquals(listOf("root", "solo"), sources.map(::feedSourceKey))
    }

    @Test
    fun `hidden from feeds follows the conversation`() {
        val sources = groupFeedSources(listOf(moved, still)) { false }
        assertEquals(listOf("solo"), withoutHidden(sources, setOf("root")).map { it.idHex })
        // The group the conversation lives in now is not what was hidden.
        assertEquals(2, withoutHidden(sources, setOf("head")).size)
        assertEquals(listOf("head"), hiddenSources(sources, linkedSetOf("root")).map { it.idHex })
    }
}
