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

    // Seen live 2026-11-12: the notification and the row read the message
    // text, which is the `/g/` link with its secret after `#`.
    @Test
    fun `a notification for an invitation card reads like the card, never the link`() {
        val link = "https://test.lakko.me/g/AAAAAAAA#fakesecretxyz1"
        val card = byteArrayOf(1, 2, 3)
        assertEquals(
            "Invitation to publish in Walk 22 channel",
            invitationAwarePreviewText(link, card, strings) { ChannelInviteSummary("Walk 22 channel", publisher = true) },
        )
        // A card that does not open never falls back to the link.
        assertEquals("New message", invitationAwarePreviewText(link, card, strings) { null })
        // A message with no card is its text.
        assertEquals("hello", invitationAwarePreviewText("hello", null, strings) { error("not called") })
    }
}
