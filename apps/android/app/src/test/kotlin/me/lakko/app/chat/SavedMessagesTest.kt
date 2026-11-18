// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.chat.AttachmentBridgeException
import me.lakko.app.media.MediaFailure
import me.lakko.app.media.isUploadLimit
import me.lakko.app.media.mediaFailureOf
import me.lakko.app.storage.BlobPassKind
import me.lakko.app.storage.StorageLimitError
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.FolderConversationKind
import uniffi.lakko_ffi_uniffi.FolderConversationRef
import uniffi.lakko_ffi_uniffi.SavedMessageView
import uniffi.lakko_ffi_uniffi.ThumbRef

/** docs/ux/saved-messages-and-file-lifetime.md sections 2-4 — the saved
 *  list's rows and the media failure classification. */
class SavedMessagesTest {
    private val conv = FolderConversationRef(FolderConversationKind.GROUP, byteArrayOf(0x0a, 0x0b))

    private fun view(id: Long, at: Long, kind: String = "", excerpt: String = "hi") = SavedMessageView(
        hlc = 1u,
        conversation = conv,
        messageId = id.toULong(),
        savedAtMs = at.toULong(),
        author = "Ann",
        excerpt = excerpt,
        attachmentKind = kind,
        blobIds = emptyList(),
    )

    private fun ref(id: Int, mime: String, kind: String? = null, thumb: Int? = null) = AttachmentRef(
        blobId = ByteArray(32) { id.toByte() },
        key = ByteArray(32),
        nonce = ByteArray(12),
        mime = mime,
        size = 10u,
        name = "doc.pdf",
        kind = kind,
        thumb = thumb?.let { ThumbRef(ByteArray(32) { t -> it.toByte() }, ByteArray(32), ByteArray(12), "image/jpeg", 5u, null, null) },
    )

    private fun msg(id: Long, vararg refs: AttachmentRef, text: String = "", deleted: Boolean = false) = ChatMessage(
        messageId = id,
        direction = Direction.Incoming,
        text = text,
        attachments = refs.toList(),
        createdAt = "2026-10-26T10:00:00Z",
        deleted = deleted,
    )

    @Test
    fun rows_are_most_recently_saved_first_and_ties_are_stable() {
        val rows = savedRows(listOf(view(1, 100), view(3, 300), view(2, 300))) { _, _ -> null }
        assertEquals(listOf(3L, 2L, 1L), rows.map { it.messageId })
        assertEquals("0a0b", rows.first().conversationHex)
    }

    @Test
    fun a_row_not_held_is_drawn_from_its_snapshot_and_does_not_open() {
        val held = msg(2, text = "hello")
        val rows = savedRows(listOf(view(1, 100), view(2, 200))) { _, id -> if (id == 2L) held else null }
        assertTrue(rows.first { it.messageId == 2L }.opens)
        val snapshot = rows.first { it.messageId == 1L }
        assertFalse(snapshot.opens)
        assertEquals("hi", snapshot.excerpt)
        // A held message that was deleted is not opened either.
        val gone = savedRows(listOf(view(2, 200))) { _, _ -> msg(2, deleted = true) }
        assertFalse(gone.single().opens)
    }

    @Test
    fun the_snapshot_names_the_kind_and_a_files_name() {
        assertEquals("photo", savedAttachmentKind(msg(1, ref(1, "image/jpeg"))))
        assertEquals("video", savedAttachmentKind(msg(1, ref(1, "video/mp4"))))
        assertEquals("voice", savedAttachmentKind(msg(1, ref(1, "audio/mp4", kind = "voice"))))
        assertEquals("video_note", savedAttachmentKind(msg(1, ref(1, "video/mp4", kind = "video_note"))))
        assertEquals("gif", savedAttachmentKind(msg(1, ref(1, "image/gif", kind = "gif"))))
        assertEquals("file", savedAttachmentKind(msg(1, ref(1, "application/pdf"))))
        assertEquals("", savedAttachmentKind(msg(1, text = "t")))
        assertEquals("doc.pdf", savedExcerpt(msg(1, ref(1, "application/pdf"))))
        assertEquals("caption", savedExcerpt(msg(1, ref(1, "image/jpeg"), text = "caption")))
        // Base scenario 26: a channel invitation is saved as its card's line,
        // never as its text (the `/g/` link with its secret).
        val invite = msg(1, text = "https://test.lakko.me/g/AAAAAAAA#fakesecretxyz1")
            .copy(channelInvite = byteArrayOf(1, 2, 3))
        assertEquals("Invitation to Walk 26", savedExcerpt(invite) { if (it != null) "Invitation to Walk 26" else null })
        assertEquals("saved.kind.file", savedKindText("file", "doc.pdf")?.key)
        assertNull(savedKindText("", "x"))
    }

    @Test
    fun blob_ids_cover_files_and_thumbnails_once() {
        val ids = messageBlobIds(msg(1, ref(1, "video/mp4", thumb = 2), ref(1, "video/mp4")))
        assertEquals(listOf(1.toByte(), 2.toByte()), ids.map { it[0] })
    }

    @Test
    fun the_row_date_is_the_messages_own_time() {
        assertEquals(500L, savedRowDateMs(sentAtMs = 500, heldAtMs = 700, savedAtMs = 900))
        assertEquals(700L, savedRowDateMs(sentAtMs = 0, heldAtMs = 700, savedAtMs = 900))
        assertEquals(900L, savedRowDateMs(sentAtMs = 0, heldAtMs = null, savedAtMs = 900))
        val rows = savedRows(listOf(view(1, 100).copy(sentAtMs = 42u))) { _, _ -> null }
        assertEquals(42L, rows.single().dateMs)
    }

    private fun thread(head: String, messages: List<ChatMessage>, older: List<String> = emptyList()) = ChatThread(
        groupIdHex = head,
        messages = messages,
        cursor = 0,
        conversationHex = older.firstOrNull() ?: head,
        olderSegments = older,
        olderSegmentIndices = older.indices.toList(),
        segmentIndex = older.size,
    )

    @Test
    fun a_message_keeps_its_register_when_the_conversation_moves_on() {
        // Saved while "aa" was the conversation's current group.
        val before = thread("aa", listOf(msg(7, text = "hello")))
        val key = savedKeyOf("aa", before.messages.single())
        assertEquals(SavedKey("aa", 7), key)
        // The conversation moved to "bb": the same message is now an older
        // segment's row, with a negative local id, and is found by the SAME key.
        val moved = historyRowOf(msg(7, text = "hello"), segmentHex = "aa", distance = 1)
        val after = thread("bb", listOf(moved, msg(7, text = "new head message")), older = listOf("aa"))
        assertTrue(moved.messageId < 0)
        assertEquals(key, savedKeyOf("bb", moved))
        assertEquals("hello", rowForSavedKey(after, key!!)?.text)
        // The head's own message 7 is a different message: another key.
        assertEquals(SavedKey("bb", 7), savedKeyOf("bb", after.messages[1]))
        assertEquals("new head message", rowForSavedKey(after, SavedKey("bb", 7))?.text)
    }

    @Test
    fun an_older_segment_row_saves_under_its_own_segment_and_the_title_resolves_through_the_lineage() {
        val older = historyRowOf(msg(3, text = "old"), segmentHex = "AA", distance = 1)
        assertEquals(SavedKey("aa", 3), savedKeyOf("bb", older))
        val conv = thread("bb", listOf(older), older = listOf("aa"))
        // The conversation that holds segment "aa" is the one whose head is "bb".
        assertEquals("bb", conversationThreadOf(listOf(conv), "aa")?.groupIdHex)
        // A row with no DS id is not savable.
        assertNull(savedKeyOf("bb", msg(0, text = "pending")))
    }

    @Test
    fun delete_for_everyone_removal_matches_by_segment() {
        val inAa = view(5, 1).copy(conversation = FolderConversationRef(FolderConversationKind.GROUP, byteArrayOf(0xaa.toByte())))
        val inBb = view(5, 1).copy(conversation = FolderConversationRef(FolderConversationKind.GROUP, byteArrayOf(0xbb.toByte())))
        assertEquals(listOf(inAa), savedDeletedIn(listOf(inAa, inBb), "aa", setOf(5L)))
        assertTrue(savedDeletedIn(listOf(inAa, inBb), "aa", setOf(6L)).isEmpty())
    }

    @Test
    fun the_client_limit_refuses_past_it() {
        assertTrue(saveAllowed(1999, 1, 2000))
        assertFalse(saveAllowed(2000, 1, 2000))
        assertFalse(saveAllowed(1990, 32, 2000))
    }

    @Test
    fun is_saved_matches_conversation_and_id() {
        val list = listOf(view(5, 1))
        assertTrue(isSavedIn(list, SavedKey("0a0b", 5)))
        assertFalse(isSavedIn(list, SavedKey("0a0b", 6)))
        assertFalse(isSavedIn(list, SavedKey("ffff", 5)))
    }

    @Test
    fun failures_are_told_apart_through_their_causes() {
        assertEquals(MediaFailure.GONE, mediaFailureOf(AttachmentBridgeException.Missing(ByteArray(32))))
        assertEquals(
            MediaFailure.LIMIT,
            mediaFailureOf(AttachmentBridgeException.Download(StorageLimitError(BlobPassKind.READ))),
        )
        assertEquals(MediaFailure.FAILED, mediaFailureOf(AttachmentBridgeException.Download(RuntimeException("x"))))
        assertTrue(isUploadLimit(AttachmentBridgeException.Upload(StorageLimitError(BlobPassKind.UPLOAD))))
        assertFalse(isUploadLimit(AttachmentBridgeException.Upload(StorageLimitError(BlobPassKind.READ))))
        assertEquals(
            "chat.attachment.limit.send",
            me.lakko.app.media.attachFailureMessage("a.jpg", AttachmentBridgeException.Upload(StorageLimitError(BlobPassKind.UPLOAD))).key,
        )
    }
}
