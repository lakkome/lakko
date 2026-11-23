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

    private fun ref(kind: String?, mime: String?) = AttachmentRef(
        blobId = ByteArray(32),
        key = ByteArray(32),
        nonce = ByteArray(12),
        mime = mime,
        size = 1u,
        name = null,
        kind = kind,
    )

    private val voice = ref(KIND_VOICE, "audio/mp4")
    private val photo = ref(null, "image/jpeg")

    @Test
    fun aLoneAttachmentIsTheSoleBubble() {
        assertEquals(MediaBubble.VOICE, soleMediaBubble(false, "", listOf(voice), fake))
        assertEquals(MediaBubble.PHOTO, soleMediaBubble(false, "", listOf(photo), fake))
        assertEquals(MediaBubble.FILE, soleMediaBubble(false, "", listOf(ref(null, "application/pdf")), fake))
    }

    /**
     * The "nothing but" half of [soleMediaBubble]. A voice message with a
     * caption, or beside a second attachment, has a paragraph to sit under and
     * keeps the bubble's own meta row; only the lone one takes a slot of its
     * own. A tombstone shows neither.
     */
    @Test
    fun anythingElseInTheBubbleMeansNoSoleBubble() {
        assertNull(soleMediaBubble(false, "listen to this", listOf(voice), fake))
        assertNull(soleMediaBubble(false, "", listOf(voice, photo), fake))
        assertNull(soleMediaBubble(false, "", emptyList(), fake))
        assertNull(soleMediaBubble(true, "", listOf(voice), fake))
        // And each of those keeps the row under the content.
        assertEquals(
            BubbleMetaSlot.BUBBLE_ROW,
            bubbleMetaSlot(soleMediaBubble(false, "listen to this", listOf(voice), fake)),
        )
    }

    /**
     * UX spec §2 — a lone voice message is a ROW, so the message's own time
     * shares the quiet line under the waveform. It used to fall through to
     * [BubbleMetaSlot.BUBBLE_ROW] and get a second row below the player, which
     * made the bubble taller than it needed to be and read as two unrelated
     * facts: the length of the recording on one level, the time it was sent on
     * another.
     */
    @Test
    fun aLoneVoiceMessageCarriesItsTimeOnItsOwnLine() {
        assertEquals(BubbleMetaSlot.VOICE_LINE, bubbleMetaSlot(MediaBubble.VOICE))
        assertEquals(
            BubbleMetaSlot.VOICE_LINE,
            bubbleMetaSlot(soleMediaBubble(false, "", listOf(voice), fake)),
        )
    }

    @Test
    fun everyBubbleShapeHasExactlyOnePlaceForItsMeta() {
        // A lone photo or video is edge-to-edge with no chrome to carry a row,
        // so the pill on the media; a video note is a chromeless circle, so
        // under it; everything else keeps the row inside the bubble.
        assertEquals(BubbleMetaSlot.MEDIA_PILL, bubbleMetaSlot(MediaBubble.PHOTO))
        assertEquals(BubbleMetaSlot.MEDIA_PILL, bubbleMetaSlot(MediaBubble.VIDEO))
        assertEquals(BubbleMetaSlot.NOTE_PILL, bubbleMetaSlot(MediaBubble.VIDEO_NOTE))
        assertEquals(BubbleMetaSlot.BUBBLE_ROW, bubbleMetaSlot(MediaBubble.GIF))
        assertEquals(BubbleMetaSlot.BUBBLE_ROW, bubbleMetaSlot(MediaBubble.FILE))
        assertEquals(BubbleMetaSlot.BUBBLE_ROW, bubbleMetaSlot(null))
        // All four slots are reachable, so none of the four renderers in
        // `ChatScreen` / `MediaBubbles` is dead code waiting to rot.
        assertEquals(
            BubbleMetaSlot.entries.toSet(),
            MediaBubble.entries.map(::bubbleMetaSlot).toSet() + bubbleMetaSlot(null),
        )
        // And nothing maps the voice row back to the row below it — the defect.
        assertFalse(MediaBubble.entries.none { bubbleMetaSlot(it) == BubbleMetaSlot.VOICE_LINE })
        // A new bubble shape cannot silently inherit a slot: `bubbleMetaSlot`
        // is an exhaustive `when` with no `else`, so the compiler demands one.
    }
}
