// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import me.lakko.app.media.InlineKind
import me.lakko.app.media.KIND_GIF
import me.lakko.app.media.KIND_VIDEO_NOTE
import me.lakko.app.media.KIND_VOICE
import me.lakko.app.media.MediaAllowlist
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.AttachmentRef

/**
 * `docs/ux/feed.md` section 3 — which attachments an item draws as the chat's
 * bubbles, and what is left as its text line. The allowlist is a fake: host
 * tests load no native core.
 */
class FeedItemMediaTest {
    private val allowlist: MediaAllowlist = { mime ->
        when (mime) {
            "audio/mp4" -> InlineKind.AUDIO
            "video/mp4" -> InlineKind.VIDEO
            "image/gif", "image/jpeg" -> InlineKind.IMAGE
            else -> null
        }
    }
    private val strings = EnglishForTests.strings
    private val source = FeedSource(FeedSourceKind.GROUP, "aa".repeat(32), "Team", muted = false, audience = 4)

    private fun attachment(kind: String?, mime: String?): FeedAttachment = FeedAttachment(
        kind,
        mime,
        AttachmentRef(
            blobId = ByteArray(32),
            key = ByteArray(32),
            nonce = ByteArray(12),
            mime = mime,
            size = 1u,
            name = null,
            kind = kind,
        ),
    )

    private val photo = attachment(null, "image/jpeg")
    private val video = attachment(null, "video/mp4")
    private val voice = attachment(KIND_VOICE, "audio/mp4")
    private val videoNote = attachment(KIND_VIDEO_NOTE, "video/mp4")
    private val gif = attachment(KIND_GIF, "image/gif")
    private val file = attachment(null, "application/pdf")

    private fun message(text: String, vararg attachments: FeedAttachment) = FeedMessage(
        messageId = 1,
        direction = "incoming",
        text = text,
        attachments = attachments.toList(),
        createdAt = "2026-10-31T10:00:00Z",
    )

    private fun body(message: FeedMessage): String =
        itemBodyOf(FeedCandidateRef(source, message, mentionsMe = false, repliesToMe = false), strings, allowlist)

    @Test
    fun everyMediaKindCountsAndKeepsMessageOrder() {
        val refs = mediaRefsOf(message("", voice, photo, gif, video, videoNote), allowlist)
        assertEquals(listOf(voice.ref, photo.ref, gif.ref, video.ref, videoNote.ref), refs)
        assertSame(voice.ref, refs[0])
    }

    @Test
    fun aFileIsNotMedia() {
        assertTrue(mediaRefsOf(message("", file), allowlist).isEmpty())
        assertEquals(listOf(photo.ref), mediaRefsOf(message("", file, photo), allowlist))
    }

    @Test
    fun theSenderChosenKindAloneIsNotMedia() {
        // ADR-0047 section 1 — the allowlist decides, so the bytes go to no
        // player; the same rule the timeline's bubble runs.
        assertTrue(mediaRefsOf(message("", attachment(KIND_VOICE, "text/html")), allowlist).isEmpty())
        assertTrue(mediaRefsOf(message("", attachment("unknown", "image/jpeg")), allowlist).isEmpty())
        assertTrue(mediaRefsOf(message("", attachment(null, null)), allowlist).isEmpty())
    }

    @Test
    fun anAttachmentWithNoBlobCannotDrawABubble() {
        val noBlob = FeedAttachment(null, "image/jpeg")
        assertTrue(mediaRefsOf(message("", noBlob), allowlist).isEmpty())
        assertEquals(en("chat.media.photo"), body(message("", noBlob)))
    }

    @Test
    fun aMediaOnlyMessageHasNoTextLine() {
        assertEquals("", body(message("", photo)))
        assertEquals("", body(message("  \n", voice)))
        assertEquals("", body(message("", gif, videoNote)))
    }

    @Test
    fun wordsAreTheTextLineWhateverIsAttached() {
        assertEquals("look", body(message("look", photo)))
        assertEquals("look", body(message("look", file)))
    }

    @Test
    fun aFileStaysTheOneLineText() {
        assertEquals(en("chat.media.file"), body(message("", file)))
    }

    @Test
    fun aFileBesideMediaIsWhatTheTextLineSays() {
        // The photo is its bubble, so the line names the file alone, not
        // "2 attachments".
        assertEquals(en("chat.media.file"), body(message("", photo, file)))
    }

    @Test
    fun aRowWithNoBubblesStillSaysWhatTheMessageIs() {
        // A folded reply and the editor's preview draw no media.
        val ref = FeedCandidateRef(source, message("", photo), mentionsMe = false, repliesToMe = false)
        assertEquals(en("chat.media.photo"), bodyOf(ref, strings))
    }
}
