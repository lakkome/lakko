// SPDX-License-Identifier: AGPL-3.0-or-later

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * docs/ux/attachments.md 2.5 — which pasted or keyboard-inserted items attach
 * as a photo or video, and which stay with the text field.
 */
class PastedMediaTest {

    @Test
    fun photosAndVideosAttach() {
        for (mime in listOf("image/png", "image/jpeg", "image/gif", "image/webp", "image/heic", "video/mp4", "video/quicktime")) {
            assertTrue(mime, pasteAttachesMime(mime))
        }
    }

    @Test
    fun caseAndParametersDoNotMatter() {
        assertTrue(pasteAttachesMime("IMAGE/PNG"))
        assertTrue(pasteAttachesMime(" video/mp4 ; codecs=avc1 "))
    }

    @Test
    fun textAndOtherFilesStayText() {
        for (mime in listOf("text/plain", "text/html", "application/pdf", "audio/mp4", "text/uri-list", "application/octet-stream")) {
            assertFalse(mime, pasteAttachesMime(mime))
        }
    }

    @Test
    fun svgIsNotAPhoto() {
        assertFalse(pasteAttachesMime("image/svg+xml"))
    }

    @Test
    fun missingOrMalformedTypesNeverAttach() {
        for (mime in listOf(null, "", "   ", "image", "image/", "/png")) {
            assertFalse(mime.toString(), pasteAttachesMime(mime))
        }
    }

    @Test
    fun theProviderTypeWinsOverTheClip() {
        assertEquals("video/mp4", resolvePastedMime("video/mp4", listOf("image/png")))
        assertEquals("text/plain", resolvePastedMime("text/plain", listOf("image/png")))
    }

    @Test
    fun withoutAProviderTypeOneAttachableClipTypeDecides() {
        assertEquals("image/gif", resolvePastedMime(null, listOf("image/gif")))
        assertEquals("image/png", resolvePastedMime("", listOf("text/plain", "IMAGE/PNG")))
    }

    @Test
    fun aClipNamingSeveralMediaTypesDoesNotGuess() {
        assertNull(resolvePastedMime(null, listOf("image/png", "video/mp4")))
    }

    @Test
    fun aClipNamingNoMediaTypeGivesNoType() {
        assertNull(resolvePastedMime(null, listOf("text/plain", "text/html")))
        assertNull(resolvePastedMime(null, emptyList()))
    }

    @Test
    fun theSameTypeListedTwiceStillDecides() {
        assertEquals("image/jpeg", resolvePastedMime(null, listOf("image/jpeg", "image/jpeg")))
    }
}
