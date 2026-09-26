// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.media.KIND_GIF
import me.lakko.app.media.KIND_VIDEO_NOTE
import me.lakko.app.media.KIND_VOICE
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.lakko_ffi_uniffi.AttachmentRef

/**
 * ADR-0052 / `docs/ux/message-actions.md` section 3.1 — the quote a reply
 * carries, and what a bubble draws from one.
 *
 * Pure rules, so they run here with no Compose and no native core.
 */
class ReplySnapshotTest {
    private fun attachment(kind: String?, mime: String) = AttachmentRef(
        blobId = ByteArray(32) { 1 },
        key = ByteArray(32) { 2 },
        nonce = ByteArray(12) { 3 },
        mime = mime,
        size = 10u,
        name = "a",
        kind = kind,
    )

    private fun message(text: String, vararg attachments: AttachmentRef) = ChatMessage(
        messageId = 42L,
        direction = Direction.Incoming,
        text = text,
        attachments = attachments.toList(),
        createdAt = "1970-01-01T00:00:00Z",
        authorKey = "Ym9i",
    )

    @Test
    fun `the excerpt is the text trimmed to one line and the target is the id`() {
        val ref = replySnapshotFor(message("  first line\n\n  second   line  "), "Anna")
        assertEquals(42L, ref.target)
        assertEquals("Anna", ref.author)
        assertEquals("first line second line", ref.excerpt)
    }

    @Test
    fun `a message with no text names the kind of its first attachment`() {
        assertEquals(
            "Voice message",
            replySnapshotFor(message("", attachment(KIND_VOICE, "audio/mp4")), "Anna").excerpt,
        )
        assertEquals(
            "Video message",
            replySnapshotFor(message("", attachment(KIND_VIDEO_NOTE, "video/mp4")), "Anna").excerpt,
        )
        assertEquals("GIF", replySnapshotFor(message("", attachment(KIND_GIF, "image/gif")), "Anna").excerpt)
        assertEquals("Photo", replySnapshotFor(message("", attachment(null, "image/jpeg")), "Anna").excerpt)
        assertEquals("Video", replySnapshotFor(message("", attachment(null, "video/mp4")), "Anna").excerpt)
        assertEquals("File", replySnapshotFor(message("", attachment(null, "application/pdf")), "Anna").excerpt)
    }

    @Test
    fun `the first attachment decides, and text wins over any attachment`() {
        val both = message("caption", attachment(KIND_VOICE, "audio/mp4"), attachment(null, "image/png"))
        assertEquals("caption", replySnapshotFor(both, "Anna").excerpt)
        val two = message("", attachment(null, "image/png"), attachment(KIND_VOICE, "audio/mp4"))
        assertEquals("Photo", replySnapshotFor(two, "Anna").excerpt)
    }

    @Test
    fun `nothing to quote carries no excerpt, and a blank name carries no name`() {
        val ref = replySnapshotFor(message("   "), "  ")
        assertNull(ref.excerpt)
        assertNull(ref.author)
    }

    @Test
    fun `the own-message label is whatever the caller says the timeline calls it`() {
        assertEquals("You", replySnapshotFor(message("hi"), "You").author)
    }

    @Test
    fun `the quote lines fall back to an empty name and Message`() {
        assertEquals("" to "Message", replyQuoteLines(ReplyRef(target = 1L, author = null, excerpt = null)))
        assertEquals("" to "Message", replyQuoteLines(ReplyRef(target = 1L, author = " ", excerpt = "")))
        assertEquals("Anna" to "hello", replyQuoteLines(ReplyRef(target = 1L, author = "Anna", excerpt = "hello")))
    }
}
