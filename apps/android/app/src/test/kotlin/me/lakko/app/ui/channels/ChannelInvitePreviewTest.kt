// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The Chats row of a conversation whose last message is a channel
 * invitation reads like the card, never as the link.
 *
 * Seen on a live walk: the row showed `https://test.lakko.me/g/...`.
 */
class ChannelInvitePreviewTest {
    private val strings = EnglishForTests.install()

    @Test
    fun `a subscriber invitation names the channel`() {
        assertEquals(
            "Invitation to Pixel Pub Walk",
            channelInvitePreviewText(ChannelInviteSummary("Pixel Pub Walk", publisher = false), strings),
        )
    }

    @Test
    fun `a publisher invitation says it invites to publish`() {
        assertEquals(
            "Invitation to publish in Pixel Pub Walk",
            channelInvitePreviewText(ChannelInviteSummary("Pixel Pub Walk", publisher = true), strings),
        )
    }
}
