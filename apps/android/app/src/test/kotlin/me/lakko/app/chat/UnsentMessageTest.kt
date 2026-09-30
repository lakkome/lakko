// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.ui.chat.TimelineItem
import me.lakko.app.ui.chat.statusLabel
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * A message appears in the conversation when the person writes it, and
 * settles afterwards.
 *
 * Before this the send was awaited first and the row appended only on
 * success, so a message written on a bad connection left the person looking
 * at an error over an unchanged composer, with nothing in the conversation at
 * all. The words were safe; the message was not there, and there was nothing
 * to retry.
 */
class UnsentMessageTest {
    init {
        EnglishForTests.install()
    }


    private fun message(
        ack: Ack,
        messageId: Long = 0L,
        localId: Long = -1L,
    ) = ChatMessage(
        messageId = messageId,
        direction = Direction.Outgoing,
        text = "the words",
        createdAt = "1970-01-01T00:00:00Z",
        ack = ack,
        localId = localId,
    )

    // ----- what the row says -------------------------------------------

    @Test
    fun `a pending row says it is sending`() {
        assertEquals("Sending...", statusLabel(Ack.PENDING, "17:21"))
    }

    @Test
    fun `a failed row says it is not sent`() {
        // The same word the media rows, iOS and the web client use. Two
        // spellings of "this did not send" would be one of them being wrong.
        assertEquals("Not sent", statusLabel(Ack.FAILED, "17:21"))
    }

    @Test
    fun `a sent row shows its time`() {
        assertEquals("17:21", statusLabel(Ack.SENT, "17:21"))
    }

    @Test
    fun `a sent row with no time shows nothing`() {
        assertNull(statusLabel(Ack.SENT, null))
    }

    // ----- identity ----------------------------------------------------

    @Test
    fun `a local row is identified by its localId`() {
        // Two unsent rows both have messageId 0, so without this they share a
        // lazy-list key and one row is drawn for two messages.
        assertEquals(-1L, message(Ack.PENDING, localId = -1L).rowId)
        assertNotEquals(
            message(Ack.PENDING, localId = -1L).rowId,
            message(Ack.PENDING, localId = -2L).rowId,
        )
    }

    @Test
    fun `the identity does not move when the send lands`() {
        // An identity that changed at that moment would tear the row down and
        // rebuild it, mid-send.
        val pending = message(Ack.PENDING, localId = -1L)
        val landed = message(Ack.SENT, messageId = 4321L, localId = -1L)
        assertEquals(pending.rowId, landed.rowId)
        assertEquals(4321L, landed.messageId)
    }

    @Test
    fun `a row from the server is identified by its messageId`() {
        assertEquals(9L, message(Ack.SENT, messageId = 9L, localId = 0L).rowId)
    }

    @Test
    fun `the lazy-list key follows the identity, not the message id`() {
        val row = TimelineItem.Row(
            message = message(Ack.PENDING, localId = -7L),
            firstInRun = true,
            lastInRun = true,
            time = null,
        )
        assertEquals("m--7", row.key)
    }

    // ----- what reaches the disk ---------------------------------------

    @Test
    fun `an unsent row is not written`() {
        val rows = persistableMessages(
            listOf(
                message(Ack.SENT, messageId = 1L, localId = 0L),
                message(Ack.PENDING),
                message(Ack.FAILED, localId = -2L),
            ),
        )
        assertEquals(listOf(1L), rows.map { it.messageId })
    }

    @Test
    fun `an ordinary transcript is written whole`() {
        // The control: filtering on the wrong condition would silently write
        // nothing at all.
        val rows = persistableMessages(
            listOf(
                message(Ack.SENT, messageId = 1L, localId = 0L),
                message(Ack.SENT, messageId = 2L, localId = 0L),
            ),
        )
        assertEquals(2, rows.size)
    }

    @Test
    fun `a row from the journal is sent without saying so`() {
        // Everything off the journal, the message log or a call transcript
        // exists BECAUSE it was delivered, so the default carries them.
        val incoming = ChatMessage(
            messageId = 5L,
            direction = Direction.Incoming,
            text = "hi",
            createdAt = "1970-01-01T00:00:00Z",
        )
        assertEquals(Ack.SENT, incoming.ack)
        assertEquals(1, persistableMessages(listOf(incoming)).size)
    }
}
