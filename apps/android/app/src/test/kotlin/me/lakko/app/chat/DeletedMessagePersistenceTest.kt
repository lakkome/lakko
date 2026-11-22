// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.serialization.json.Json
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.ui.chat.previewLine
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.AttachmentRef

/**
 * ADR-0023 — what a deleted message leaves on disk.
 *
 * The tombstone is drawn from the `deleted` flag, by the bubble and by the
 * conversation row alike, so the text and the attachments of a deleted message
 * are read by nothing. Storing them anyway kept the words someone chose to
 * delete - and the content keys that open their attachment blobs on the
 * server - on the device for as long as the transcript lived.
 *
 * Pure JSON work, so it runs here with no Keystore and no `Context`.
 */
class DeletedMessagePersistenceTest {
    init {
        EnglishForTests.install()
    }


    private val bob = "Ym9i"

    private fun attachment() = AttachmentRef(
        blobId = ByteArray(32) { 1 },
        key = ByteArray(32) { 2 },
        nonce = ByteArray(12) { 3 },
        mime = "image/png",
        size = 10u,
        name = "a.png",
    )

    private fun message(deleted: Boolean) = ChatMessage(
        messageId = 7L,
        direction = Direction.Incoming,
        text = "the secret",
        attachments = listOf(attachment()),
        createdAt = "1970-01-01T00:00:00Z",
        authorKey = bob,
        deleted = deleted,
        reactions = listOf(Reaction(reactor = bob, emoji = "👍")),
    )

    @Test
    fun `a deleted message is stored with no text and no attachments`() {
        val stored = PersistedMessage.from(message(deleted = true))
        assertTrue(stored.deleted)
        assertEquals("", stored.text)
        assertTrue(stored.attachments.isEmpty())
    }

    @Test
    fun `a message that was not deleted keeps both`() {
        // The control. Emptying on the wrong condition would silently erase
        // every transcript, which is why this pair runs together.
        val stored = PersistedMessage.from(message(deleted = false))
        assertEquals("the secret", stored.text)
        assertEquals(1, stored.attachments.size)
    }

    @Test
    fun `the deleted message still restores as a deleted message`() {
        // Emptying the row must not make it unreadable, or a deleted message
        // would come back as an ordinary empty bubble.
        val blob = Json.encodeToString(
            PersistedMessage.serializer(),
            PersistedMessage.from(message(deleted = true)),
        )
        val restored = Json.decodeFromString(PersistedMessage.serializer(), blob).toChatMessage()
        assertNotNull(restored)
        assertTrue(restored!!.deleted)
        assertEquals("", restored.text)
    }

    @Test
    fun `reactions survive a delete`() {
        // The message is gone; who reacted to it is still the group's state,
        // and the tombstone carries the chips.
        val stored = PersistedMessage.from(message(deleted = true))
        assertEquals(
            listOf(PersistedReaction(reactorKeyB64 = bob, emoji = "👍")),
            stored.reactions,
        )
    }

    @Test
    fun `the conversation row never previews a deleted message`() {
        // docs/ux/message-actions.md section 9 — a deleted message has no
        // preview line; the row is taken from the newest message that is not
        // deleted. The preview never looked at the text of a deleted message.
        assertEquals(
            "",
            previewLine(text = "gone", attachments = emptyList(), deleted = true, author = null),
        )
    }
}
