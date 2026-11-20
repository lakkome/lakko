// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import me.lakko.app.chat.ConversationListRow
import me.lakko.app.chat.ConversationRowKind
import me.lakko.app.devices.sync.FlagsState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Base scenario 29 (2026-11-13): a channel's row in Chats offered only "Add
 * to folder". UX group-and-channel-privacy 12 — the row behaves as a group's,
 * its pin, mute and archive the synced flags keyed on the channel id.
 */
class ChannelFlagsTest {
    private val id = "AB".repeat(16)

    private fun flags(pinned: Boolean = false, muted: Boolean = false, archived: Boolean = false) =
        FlagsState(pinned = pinned, muted = muted, archived = archived, unreadOverride = false, updatedAtHlc = 1u)

    private val row = ConversationListRow(
        id = id,
        kind = ConversationRowKind.CHANNEL,
        title = "News",
        preview = "",
        time = null,
        lastActivityMs = 1L,
    )

    @Test
    fun `a channel's flags are read under its lower-case id`() {
        val map = mapOf("ab".repeat(16) to flags(pinned = true, muted = true))
        assertEquals("ab".repeat(16), ChannelFlags.key(id))
        assertEquals(flags(pinned = true, muted = true), ChannelFlags.of(map, id))
        assertNull(ChannelFlags.of(emptyMap(), id))
    }

    @Test
    fun `the row carries pin, mute and archive`() {
        val flagged = row.withChannelFlags(flags(pinned = true, muted = true, archived = true))
        assertTrue(flagged.pinned && flagged.muted && flagged.archived)
        val plain = row.withChannelFlags(null)
        assertFalse(plain.pinned || plain.muted || plain.archived)
    }
}
