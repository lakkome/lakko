// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import me.lakko.app.chat.AttachmentBridge
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.AttachmentRef

/**
 * ADR-0047 §1 — inline playback is decided by the MIME allowlist, never by the
 * sender-chosen `kind`. The allowlist is a fake here (the native core is not
 * loaded in host tests); it mirrors the core's parameter-ignoring, SVG-free
 * shape so the decision rule is what is under test.
 */
class InlineMediaTest {

    private val calls = ArrayList<String>()
    private val fake: MediaAllowlist = { mime ->
        calls += mime
        when (mime.substringBefore(';').trim().lowercase()) {
            "audio/mp4" -> InlineKind.AUDIO
            "video/mp4" -> InlineKind.VIDEO
            "image/gif", "image/webp", "image/png", "image/jpeg" -> InlineKind.IMAGE
            else -> null
        }
    }

    @Test
    fun matchingKindAndClassPickTheBubble() {
        assertEquals(MediaBubble.VOICE, renderDecision(KIND_VOICE, "audio/mp4", fake).bubble)
        assertEquals(MediaBubble.VIDEO_NOTE, renderDecision(KIND_VIDEO_NOTE, "video/mp4", fake).bubble)
        assertEquals(MediaBubble.GIF, renderDecision(KIND_GIF, "image/gif", fake).bubble)
        assertEquals(MediaBubble.GIF, renderDecision(KIND_GIF, "video/mp4", fake).bubble)
    }

    @Test
    fun kindAloneNeverUnlocksInlinePlayback() {
        val d = renderDecision(KIND_VOICE, "text/html", fake)
        assertEquals(MediaBubble.FILE, d.bubble)
        assertNull(d.inline)
        assertEquals(MediaBubble.FILE, renderDecision(KIND_VOICE, null, fake).bubble)
        assertEquals(MediaBubble.FILE, renderDecision(KIND_GIF, "image/svg+xml", fake).bubble)
    }

    @Test
    fun mismatchedClassIsAFileChip() {
        // A voice kind carrying a video type is not a voice bubble.
        val d = renderDecision(KIND_VOICE, "video/mp4", fake)
        assertEquals(MediaBubble.FILE, d.bubble)
        assertEquals(InlineKind.VIDEO, d.inline)
        assertEquals(MediaBubble.FILE, renderDecision(KIND_VIDEO_NOTE, "audio/mp4", fake).bubble)
    }

    @Test
    fun unknownKindIsAFileChipThatMayStillPreview() {
        val d = renderDecision("sticker", "image/png", fake)
        assertEquals(MediaBubble.FILE, d.bubble)
        assertEquals(InlineKind.IMAGE, d.inline)
        assertEquals(MediaBubble.FILE, renderDecision("sticker", "video/mp4", fake).bubble)
        assertEquals(MediaBubble.FILE, renderDecision(null, "audio/mp4", fake).bubble)
    }

    @Test
    fun noKindWithAnAllowlistedImageIsAPhoto() {
        for (mime in listOf("image/jpeg", "image/png", "image/gif", "image/webp", "IMAGE/JPEG; q=1")) {
            val d = renderDecision(null, mime, fake)
            assertEquals(mime, MediaBubble.PHOTO, d.bubble)
            assertEquals(InlineKind.IMAGE, d.inline)
        }
    }

    @Test
    fun noKindWithAnAllowlistedVideoIsAVideo() {
        val d = renderDecision(null, "video/mp4", fake)
        assertEquals(MediaBubble.VIDEO, d.bubble)
        assertEquals(InlineKind.VIDEO, d.inline)
    }

    @Test
    fun heicSvgAndOtherVideoTypesStayFileChips() {
        for (mime in listOf("image/heic", "image/svg+xml", "video/webm", "video/quicktime", "application/pdf")) {
            assertEquals(mime, MediaBubble.FILE, renderDecision(null, mime, fake).bubble)
        }
        assertEquals(MediaBubble.FILE, renderDecision(null, null, fake).bubble)
    }

    @Test
    fun aThrowingAllowlistNeverYieldsAPhoto() {
        val throwing: MediaAllowlist = { throw IllegalStateException("native core missing") }
        assertEquals(MediaBubble.FILE, renderDecision(null, "image/jpeg", throwing).bubble)
    }

    @Test
    fun aThrowingAllowlistFailsClosed() {
        val throwing: MediaAllowlist = { throw IllegalStateException("native core missing") }
        val d = renderDecision(KIND_VOICE, "audio/mp4", throwing)
        assertEquals(MediaBubble.FILE, d.bubble)
        assertNull(d.inline)
    }

    @Test
    fun emptyMimeNeverReachesTheAllowlist() {
        assertNull(inlineKindOf("", fake))
        assertNull(inlineKindOf(null, fake))
        assertTrue(calls.isEmpty())
    }

    @Test
    fun fileChipPreviewGoesThroughTheAllowlistToo() {
        fun ref(mime: String?) = AttachmentRef(
            blobId = ByteArray(32),
            key = ByteArray(32),
            nonce = ByteArray(12),
            mime = mime,
            size = 1u,
            name = null,
        )
        assertTrue(AttachmentBridge.isInlinePreviewable(ref("image/png"), fake))
        assertFalse(AttachmentBridge.isInlinePreviewable(ref("audio/mp4"), fake))
        assertFalse(AttachmentBridge.isInlinePreviewable(ref("image/svg+xml"), fake))
    }
}
