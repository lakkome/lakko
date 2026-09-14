// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import me.lakko.app.chat.AttachmentMediaMeta
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.ThumbRef

/**
 * UX spec §2 / §5 states — an outgoing media message shows an upload ring,
 * stays as `Not sent · Retry` on failure, and a retry RESUMES instead of
 * starting over.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class MediaSendQueueTest {

    private fun ref(meta: AttachmentMediaMeta) = AttachmentRef(
        blobId = ByteArray(32),
        key = ByteArray(32),
        nonce = ByteArray(12),
        mime = "audio/mp4",
        size = 3u,
        name = "voice.m4a",
        kind = meta.kind,
        durationMs = meta.durationMs,
        waveform = meta.waveform,
        thumb = meta.thumb,
    )

    private fun thumb() = ThumbRef(ByteArray(32), ByteArray(32), ByteArray(12), "image/jpeg", 10u)

    @Test
    fun successCarriesTheMetadataAndRemovesTheRow() = runTest {
        val published = ArrayList<AttachmentRef>()
        val queue = MediaSendQueue(
            scope = this,
            uploadThumb = { thumb() },
            uploadMain = { _, _, _, meta, progress ->
                progress(0.5f)
                ref(meta)
            },
            publish = { published += it },
        )
        val id = queue.newTempId()
        queue.enqueue(
            MediaSendQueue.Job(
                tempId = id, kind = KIND_VIDEO_NOTE, mime = "video/mp4", name = "n.mp4",
                durationMs = 2_000, waveform = null, width = 480, height = 480,
                poster = PosterFrame(byteArrayOf(1), "image/jpeg", 240, 240),
                loadBytes = { byteArrayOf(1, 2, 3) to "video/mp4" },
            ),
        )
        assertEquals(1, queue.rows.value.size)
        runCurrent()
        assertEquals(emptyList<PendingMediaRow>(), queue.rows.value)
        val sent = published.single()
        assertEquals(KIND_VIDEO_NOTE, sent.kind)
        assertEquals(2_000u, sent.durationMs)
        assertTrue(sent.thumb != null)
    }

    @Test
    fun aFailedPublishIsRetriedWithoutUploadingAgain() = runTest {
        var uploads = 0
        var publishAttempts = 0
        val queue = MediaSendQueue(
            scope = this,
            uploadThumb = { thumb() },
            uploadMain = { _, _, _, meta, _ ->
                uploads += 1
                ref(meta)
            },
            publish = {
                publishAttempts += 1
                if (publishAttempts == 1) throw IllegalStateException("DS unreachable")
            },
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
        assertTrue(queue.rows.value.single().failed)

        queue.retry(id)
        runCurrent()
        assertEquals(emptyList<PendingMediaRow>(), queue.rows.value)
        assertEquals(1, uploads)
        assertEquals(2, publishAttempts)
    }

    @Test
    fun retryIsIgnoredWhileAJobIsInFlightAndAFailedPosterStillSends() = runTest {
        var uploads = 0
        val gate = kotlinx.coroutines.CompletableDeferred<Unit>()
        val published = ArrayList<AttachmentRef>()
        val queue = MediaSendQueue(
            scope = this,
            uploadThumb = { throw IllegalStateException("poster upload failed") },
            uploadMain = { _, _, _, meta, _ ->
                uploads += 1
                gate.await()
                ref(meta)
            },
            publish = { published += it },
        )
        val id = queue.newTempId()
        queue.enqueue(
            MediaSendQueue.Job(
                tempId = id, kind = KIND_VIDEO_NOTE, mime = "video/mp4", name = "n.mp4",
                durationMs = 1_000, waveform = null, width = null, height = null,
                poster = PosterFrame(byteArrayOf(1), "image/jpeg", 1, 1),
                loadBytes = { byteArrayOf(1) to "video/mp4" },
            ),
        )
        runCurrent()
        queue.retry(id)
        runCurrent()
        assertEquals(1, uploads)
        gate.complete(Unit)
        runCurrent()
        assertEquals(emptyList<PendingMediaRow>(), queue.rows.value)
        // ADR-0047 open question, current answer: sent without a poster.
        assertEquals(null, published.single().thumb)
        assertEquals(KIND_VIDEO_NOTE, published.single().kind)
    }

    @Test
    fun aGifLoadFailureIsNotSentAndItsNameFollowsTheType() = runTest {
        var loads = 0
        val names = ArrayList<String>()
        val queue = MediaSendQueue(
            scope = this,
            uploadThumb = { thumb() },
            uploadMain = { _, _, name, meta, _ ->
                names += name
                ref(meta)
            },
            publish = {},
        )
        val id = queue.newTempId()
        queue.enqueue(
            MediaSendQueue.Job(
                tempId = id, kind = KIND_GIF, mime = "image/gif", name = "gif-1.gif",
                durationMs = null, waveform = null, width = 200, height = 100, poster = null,
                loadBytes = {
                    loads += 1
                    if (loads == 1) null else byteArrayOf(1) to "image/webp"
                },
            ),
        )
        runCurrent()
        assertTrue(queue.rows.value.single().failed)
        assertTrue(names.isEmpty())
        queue.retry(id)
        runCurrent()
        assertEquals(listOf("gif-1.webp"), names)
        assertEquals("gif-1.mp4", MediaSendQueue.gifNameFor("gif-1.gif", "video/mp4"))
    }

    @Test
    fun discardDropsAFailedRow() = runTest {
        val queue = MediaSendQueue(
            scope = this,
            uploadThumb = { thumb() },
            uploadMain = { _, _, _, _, _ -> throw IllegalStateException("storage down") },
            publish = {},
        )
        val id = queue.newTempId()
        queue.enqueue(
            MediaSendQueue.Job(
                tempId = id, kind = KIND_VOICE, mime = "audio/mp4", name = "v.m4a",
                durationMs = 900, waveform = null, width = null, height = null, poster = null,
                loadBytes = { byteArrayOf(1) to "audio/mp4" },
            ),
        )
        runCurrent()
        assertTrue(queue.rows.value.single().failed)
        queue.discard(id)
        assertEquals(emptyList<PendingMediaRow>(), queue.rows.value)
    }
}
