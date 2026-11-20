// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import androidx.compose.ui.graphics.Color
import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Base scenario 26 (2026-11-13): a private channel invitation in a
 * one-to-one showed the `/g/` link with its secret above the card on Android.
 * A bubble whose card is drawn shows the card alone, and the card's action is
 * the primary button (design system), as on iOS.
 */
class ChannelInviteBubbleTextTest {
    private val strings = EnglishForTests.install()

    @Test
    fun `a bubble whose card opens draws no link text`() {
        assertFalse(bubbleDrawsMessageText(cardDrawn = true, cardOpens = true, linkCardResolved = false))
    }

    @Test
    fun `a link sent as text alone is left out once its card is drawn`() {
        assertTrue(bubbleDrawsMessageText(cardDrawn = true, cardOpens = false, linkCardResolved = false))
        assertFalse(bubbleDrawsMessageText(cardDrawn = true, cardOpens = false, linkCardResolved = true))
    }

    @Test
    fun `a bubble that draws no card keeps its text`() {
        assertTrue(bubbleDrawsMessageText(cardDrawn = false, cardOpens = true, linkCardResolved = true))
    }

    @Test
    fun `the card line names the invitation and nothing else`() {
        val summary = ChannelInviteSummary("Walk 26 private", publisher = false)
        assertEquals("Invitation to Walk 26 private", channelInviteLine(byteArrayOf(1), strings) { summary })
        assertNull(channelInviteLine(null, strings) { summary })
        assertNull(channelInviteLine(ByteArray(0), strings) { summary })
        assertNull(channelInviteLine(byteArrayOf(1), strings) { null })
    }

    @Test
    fun `the card's action is the primary button on an incoming bubble`() {
        val accent = Color(0xFF3730A3)
        val white = Color.White
        assertEquals(CardActionColors(container = accent, label = white), cardActionColors(false, accent, white))
        // On an outgoing bubble (accent fill) the two swap so it still reads.
        assertEquals(CardActionColors(container = white, label = accent), cardActionColors(true, accent, white))
    }

    // Follow-up to scenario 26: an own message drawn as an invitation card
    // offers no Edit (it would put the link with its secret in the
    // composer); Delete stays.
    @Test
    fun `an own invitation card offers delete but never edit`() {
        val card = me.lakko.app.ui.chat.messageActionsFor(
            isOwn = true, deleted = false, text = "", menuAvailable = true, invitationCard = true,
        )
        assertFalse(card.canEdit)
        assertTrue(card.canDelete)
        assertFalse(card.canCopy)
        // Maintainer, 2026-11-13: never forwarded.
        assertFalse(card.canForward)
        val plain = me.lakko.app.ui.chat.messageActionsFor(
            isOwn = true, deleted = false, text = "hello", menuAvailable = true,
        )
        assertTrue(plain.canEdit)
        assertTrue(plain.canForward)
    }
}
