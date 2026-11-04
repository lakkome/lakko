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
        assertEquals("Anna", snapshotAuthorOf(incoming, "You", isOneToOne = false, authorName = "Anna", peerName = null))
        assertEquals("Bob", snapshotAuthorOf(incoming, "You", isOneToOne = true, authorName = null, peerName = "Bob"))
        assertNull(snapshotAuthorOf(incoming, "You", isOneToOne = false, authorName = null, peerName = "Bob"))
        assertNull(snapshotAuthorOf(incoming, "You", isOneToOne = true, authorName = null, peerName = null))
        assertEquals("You", snapshotAuthorOf(message(Direction.Outgoing), "You", false, null, null))
    }

    @Test
    fun `an unnamed author leaves the reply and the forward with no name`() {
        val incoming = message(Direction.Incoming)
        val author = snapshotAuthorOf(incoming, "You", isOneToOne = false, authorName = null, peerName = null)
        assertNull(replySnapshotFor(incoming, author.orEmpty()).author)
        assertNull(forwardPayloadOf(incoming, author, "ab".repeat(32)).second.author)
    }
}
