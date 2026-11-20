// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.chat.ConversationListRow
import me.lakko.app.chat.ConversationRowKind
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Base scenario 29 (2026-11-13): a long press on a channel row in Chats
 * showed only "Add to folder" while a group row showed Pin, Mute, Archive.
 * UX group-and-channel-privacy 12 — a channel's row offers what a group's
 * does, and acts on the synced flags under the channel id.
 */
class ChannelRowMenuTest {
    private val channel = ConversationListRow(
        id = "ab".repeat(16),
        kind = ConversationRowKind.CHANNEL,
        title = "News",
        preview = "",
        time = null,
        lastActivityMs = 1L,
    )

    private fun handlers(toggled: MutableList<Pair<String, FlagField>>) = RowHandlers(
        folders = emptyList(),
        foldersEnabled = true,
        inFolder = { _, _ -> false },
        onToggleFlag = { row, field -> toggled += row.id to field },
        onToggleFolderPin = { _, _ -> },
        onToggleMembership = { _, _, _ -> },
        onNewFolderWith = {},
        onOpenThread = {},
        onOpenChannel = {},
        quietCallOffer = null,
        onAcceptCall = {},
        onDeclineCall = {},
    )

    @Test
    fun `a channel row in All chats offers pin, mute and archive`() {
        val toggled = mutableListOf<Pair<String, FlagField>>()
        val menu = RowMenuModel(row = channel, folder = null, handlers = handlers(toggled))
        assertTrue(menu.showPin)
        assertTrue(menu.showFlags)
        menu.onPin()
        menu.onFlag(FlagField.Mute)
        menu.onFlag(FlagField.Archive)
        assertEquals(
            listOf(channel.id to FlagField.Pin, channel.id to FlagField.Mute, channel.id to FlagField.Archive),
            toggled,
        )
    }
}
