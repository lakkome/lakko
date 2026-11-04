// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.profile.ResolvedName
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** ADR-0052 — a reply quote and a forward attribution carry a real name or
 *  none: never a placeholder word, never a key fingerprint. */
class SnapshotAuthorTest {

    private fun name(primary: String, placeholder: Boolean) =
        ResolvedName(primary = primary, verifiedUsername = null, isPlaceholder = placeholder, theirName = null, usernameNotVerified = false, fingerprint = "a1b2 c3d4")

    private fun message(direction: Direction) = ChatMessage(
        messageId = 7L,
        direction = direction,
        text = "hello",
        createdAt = "2026-11-04T10:00:00Z",
    )

    @Test
    fun `a placeholder or a fingerprint is never a snapshot name`() {
        assertEquals("Anna", snapshotNameFrom(name("Anna", placeholder = false)))
        assertNull(snapshotNameFrom(name("Member", placeholder = true)))
        assertNull(snapshotNameFrom(name("  ", placeholder = false)))
    }

    @Test
    fun `the author carried is the name, the one-to-one peer, or none`() {
        val incoming = message(Direction.Incoming)
        assertEquals("Anna", snapshotAuthorOf(incoming, "Vera", isOneToOne = false, authorName = "Anna", peerName = null))
        assertEquals("Bob", snapshotAuthorOf(incoming, "Vera", isOneToOne = true, authorName = null, peerName = "Bob"))
        assertNull(snapshotAuthorOf(incoming, "Vera", isOneToOne = false, authorName = null, peerName = "Bob"))
        assertNull(snapshotAuthorOf(incoming, "Vera", isOneToOne = true, authorName = null, peerName = null))
    }

    @Test
    fun `a reply to one's own message carries the own name and reads You on the own devices`() {
        me.lakko.app.l10n.EnglishForTests.install()
        val own = message(Direction.Outgoing)
        val author = snapshotAuthorOf(own, "Vera", isOneToOne = false, authorName = null, peerName = null)
        assertEquals("Vera", author)
        val snapshot = replySnapshotFor(own, author.orEmpty())
        assertEquals("Vera", snapshot.author)
        assertEquals("Vera", forwardPayloadOf(own, author, "ab".repeat(32)).second.author)
        // Every other device draws the carried name; this account's own draw "You".
        assertEquals("Vera", replyQuoteLines(snapshot).first)
        assertEquals(me.lakko.app.l10n.EnglishForTests.strings["chat.author.you"], replyQuoteLines(snapshot, quotesOwnAccount = true).first)
        // An own message with no own name set carries none, never "You".
        assertNull(snapshotAuthorOf(own, null, isOneToOne = false, authorName = null, peerName = null))
    }

    @Test
    fun `an unnamed author leaves the reply and the forward with no name`() {
        val incoming = message(Direction.Incoming)
        val author = snapshotAuthorOf(incoming, "Vera", isOneToOne = false, authorName = null, peerName = null)
        assertNull(replySnapshotFor(incoming, author.orEmpty()).author)
        assertNull(forwardPayloadOf(incoming, author, "ab".repeat(32)).second.author)
    }
}
