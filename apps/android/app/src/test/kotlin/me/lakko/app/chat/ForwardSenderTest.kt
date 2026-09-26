// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.ThumbRef

/**
 * ADR-0052 decision 2, OQ1 — a forward re-encrypts every attachment under a
 * fresh key and uploads its own blob, keeping the metadata the bubble draws
 * from, then sends once into the target. The four effects are fakes; the
 * composition is what is under test.
 */
class ForwardSenderTest {
    private val origin = "ab".repeat(16)
    private val target = "cd".repeat(16)

    private fun ref(seed: Byte, thumb: ThumbRef? = null) = AttachmentRef(
        blobId = ByteArray(32) { seed },
        key = ByteArray(32) { (seed + 1).toByte() },
        nonce = ByteArray(12) { (seed + 2).toByte() },
        mime = "audio/mp4",
        size = 10uL,
        name = "take.m4a",
        kind = "voice",
        durationMs = 4200u,
        waveform = byteArrayOf(1, 2, 3),
        width = 640u,
        height = 480u,
        thumb = thumb,
    )

    private fun thumb() = ThumbRef(
        blobId = ByteArray(32) { 9 },
        key = ByteArray(32) { 8 },
        nonce = ByteArray(12) { 7 },
        mime = "image/jpeg",
        size = 5uL,
        width = 320u,
        height = 320u,
    )

    private fun request(vararg attachments: AttachmentRef, forward: ForwardRef? = null) = ForwardRequest(
        sourceGroupIdHex = origin,
        message = ChatMessage(
            messageId = 7L,
            direction = Direction.Incoming,
            text = "listen",
            attachments = attachments.toList(),
            createdAt = "1970-01-01T00:00:00Z",
            authorKey = "Ym9i",
            forward = forward,
        ),
        authorLabel = "Anna",
    )

    private class Sent(
        val target: String,
        val text: String,
        val attachments: List<AttachmentRef>,
        val forward: ForwardRef,
    )

    private class Fakes {
        val downloaded = mutableListOf<AttachmentRef>()
        val downloadedThumbs = mutableListOf<ThumbRef>()
        val uploadedMeta = mutableListOf<AttachmentMediaMeta?>()
        var sent: Sent? = null
        var uploads = 0
        var failThumb = false

        fun sender(): ForwardSender = ForwardSender(
            download = { downloaded += it; "plain-${it.blobId[0]}".toByteArray() },
            downloadThumb = { downloadedThumbs += it; if (failThumb) error("thumb gone") else byteArrayOf(4, 4) },
            upload = { bytes, mime, name, media ->
                uploads += 1
                uploadedMeta += media
                // A fresh blob id and key per upload, as the real bridge mints.
                AttachmentRef(
                    blobId = ByteArray(32) { (100 + uploads).toByte() },
                    key = ByteArray(32) { (50 + uploads).toByte() },
                    nonce = ByteArray(12) { uploads.toByte() },
                    mime = mime,
                    size = bytes.size.toULong(),
                    name = name,
                    kind = media?.kind,
                    durationMs = media?.durationMs,
                    waveform = media?.waveform,
                    width = media?.width,
                    height = media?.height,
                    thumb = media?.thumb,
                )
            },
            uploadThumb = { bytes, mime, width, height ->
                ThumbRef(
                    blobId = ByteArray(32) { 77 },
                    key = ByteArray(32) { 66 },
                    nonce = ByteArray(12) { 55 },
                    mime = mime,
                    size = bytes.size.toULong(),
                    width = width,
                    height = height,
                )
            },
            send = { t, text, attachments, forward -> sent = Sent(t, text, attachments, forward) },
        )
    }

    @Test
    fun `a text-only forward sends once with the payload rule applied`() = runTest {
        val fakes = Fakes()
        fakes.sender().forward(request(), target)
        val sent = fakes.sent
        assertNotNull(sent)
        assertEquals(target, sent!!.target)
        assertEquals("listen", sent.text)
        assertEquals(ForwardRef("Anna", origin, 7L), sent.forward)
        assertTrue(sent.attachments.isEmpty())
        assertEquals(0, fakes.uploads)
    }

    @Test
    fun `every attachment is downloaded, re-encrypted and uploaded under a fresh key`() = runTest {
        val fakes = Fakes()
        val a = ref(1)
        val b = ref(2)
        fakes.sender().forward(request(a, b), target)

        assertEquals(listOf(a, b), fakes.downloaded)
        assertEquals(2, fakes.uploads)
        val sent = fakes.sent!!.attachments
        assertEquals(2, sent.size)
        // OQ1: the two conversations never share a blob or a key.
        for ((sourceRef, sentRef) in listOf(a, b).zip(sent)) {
            assertFalse(sourceRef.blobId.contentEquals(sentRef.blobId))
            assertFalse(sourceRef.key.contentEquals(sentRef.key))
        }
    }

    @Test
    fun `the metadata the bubble draws from is kept`() = runTest {
        val fakes = Fakes()
        fakes.sender().forward(request(ref(1)), target)
        val meta = fakes.uploadedMeta.single()
        assertNotNull(meta)
        assertEquals("voice", meta!!.kind)
        assertEquals(4200u, meta.durationMs)
        assertArrayEquals(byteArrayOf(1, 2, 3), meta.waveform)
        assertEquals(640u, meta.width)
        assertEquals(480u, meta.height)
        val sentRef = fakes.sent!!.attachments.single()
        assertEquals("audio/mp4", sentRef.mime)
        assertEquals("take.m4a", sentRef.name)
    }

    @Test
    fun `the poster thumb is re-encrypted the same way`() = runTest {
        val fakes = Fakes()
        val source = thumb()
        fakes.sender().forward(request(ref(1, thumb = source)), target)
        assertEquals(listOf(source), fakes.downloadedThumbs)
        val sentThumb = fakes.sent!!.attachments.single().thumb
        assertNotNull(sentThumb)
        assertFalse(source.blobId.contentEquals(sentThumb!!.blobId))
        assertFalse(source.key.contentEquals(sentThumb.key))
        assertEquals("image/jpeg", sentThumb.mime)
        assertEquals(320u, sentThumb.width)
        assertEquals(320u, sentThumb.height)
    }

    @Test
    fun `a poster that cannot be fetched is dropped and the message still goes`() = runTest {
        val fakes = Fakes().apply { failThumb = true }
        fakes.sender().forward(request(ref(1, thumb = thumb())), target)
        assertNotNull(fakes.sent)
        assertNull(fakes.sent!!.attachments.single().thumb)
    }

    @Test
    fun `an attachment that cannot be fetched fails the whole forward`() = runTest {
        val fakes = Fakes()
        val sender = ForwardSender(
            download = { error("blob gone") },
            upload = { _, _, _, _ -> fail("nothing to upload"); error("unreachable") },
            send = { _, _, _, _ -> fail("must not send a forward missing its photo") },
        )
        try {
            sender.forward(request(ref(1)), target)
            fail("expected the forward to fail")
        } catch (e: IllegalStateException) {
            assertEquals("blob gone", e.message)
        }
        assertNull(fakes.sent)
    }

    @Test
    fun `forwarding a forward carries the original attribution`() = runTest {
        val fakes = Fakes()
        val first = ForwardRef("Maria", "ee".repeat(16), 12L)
        fakes.sender().forward(request(forward = first), target)
        assertEquals(first, fakes.sent!!.forward)
    }
}
