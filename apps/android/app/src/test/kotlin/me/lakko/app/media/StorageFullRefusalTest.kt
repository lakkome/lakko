// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import me.lakko.app.chat.AttachmentBridgeException
import me.lakko.app.storage.BlobPassKind
import me.lakko.app.storage.StorageFullError
import me.lakko.app.storage.StorageLimitError
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** docs/ux/storage-full.md section 2 — the server's "no room" refusal is
 *  said where the sending limit is said, with its own key. */
@OptIn(ExperimentalCoroutinesApi::class)
class StorageFullRefusalTest {
    private val full = AttachmentBridgeException.Upload(StorageFullError("/blob/ab"))
    private val limit = AttachmentBridgeException.Upload(StorageLimitError(BlobPassKind.UPLOAD))
    private val other = AttachmentBridgeException.Upload(RuntimeException("x"))

    @Test
    fun refusals_are_told_apart_through_their_causes() {
        assertTrue(isStorageFull(full))
        assertFalse(isStorageFull(limit))
        assertFalse(isUploadLimit(full))
        assertEquals(UploadRefusal.FULL, uploadRefusalOf(full))
        assertEquals(UploadRefusal.LIMIT, uploadRefusalOf(limit))
        assertNull(uploadRefusalOf(other))
    }

    @Test
    fun the_composer_says_the_server_has_no_room() {
        assertEquals("chat.attachment.full.send", attachFailureMessage("a.jpg", full).key)
        assertEquals("chat.attachment.full.send", attachFailureMessage(null, full).key)
        assertEquals("chat.attachment.limit.send", attachFailureMessage("a.jpg", limit).key)
    }

    @Test
    fun group_and_profile_photos_use_their_own_keys() {
        assertEquals("group_photo.error.full", groupPhotoRefusalKey(full))
        assertEquals("group_photo.error.limit", groupPhotoRefusalKey(limit))
        assertNull(groupPhotoRefusalKey(other))
        assertEquals("profile.save.error.full", profilePhotoUploadFailureKey(full))
        assertEquals("profile.save.error.upload", profilePhotoUploadFailureKey(other))
    }

    @Test
    fun a_full_media_send_fails_once_with_the_full_line_and_is_not_retried() = runTest {
        var uploads = 0
        val queue = MediaSendQueue(
            scope = this,
            uploadThumb = { error("no thumb") },
            uploadMain = { _, _, _, _, _ ->
                uploads += 1
                throw full
            },
            publish = { _, _, _ -> error("must not publish") },
        )
        val id = queue.newTempId()
        queue.enqueue(
            MediaSendQueue.Job(
                tempId = id, kind = KIND_VOICE, mime = "audio/mp4", name = "v.m4a",
                durationMs = 1_000, waveform = byteArrayOf(1), width = null, height = null,
                poster = null, loadBytes = { byteArrayOf(9) to "audio/mp4" },
            ),
        )
        runCurrent()
        val row = queue.rows.value.single()
        assertTrue(row.failed)
        assertEquals(UploadRefusal.FULL, row.refusal)
        assertFalse(row.limited)
        assertEquals("chat.attachment.full.send", uploadRefusalSendKey(row.refusal!!))
        assertEquals(1, uploads)
    }
}
