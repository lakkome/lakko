// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.ArticleRun

/** ADR-0090 / attachments UX 3, 6, 11 — the preview rules, the audio clock, selection styles. */
class SendPreviewTest {

    @Test
    fun `a video must be MP4 with H264 and at most 50 MB unless it goes as a file`() {
        assertNull(previewProblem(PreviewKind.VIDEO, "video/mp4", 1000, "video/avc", sendAsFile = false))
        assertEquals("attach.preview.video_refused", previewProblem(PreviewKind.VIDEO, "video/quicktime", 1000, "video/avc", false))
        assertEquals("attach.preview.video_refused", previewProblem(PreviewKind.VIDEO, "video/mp4", 1000, "video/hevc", false))
        assertNull(previewProblem(PreviewKind.VIDEO, "video/quicktime", 1000, "video/hevc", sendAsFile = true))
        assertEquals("attach.preview.file_too_big", previewProblem(PreviewKind.VIDEO, "video/mp4", PREVIEW_MAX_BYTES + 1, "video/avc", true))
        assertEquals("attach.preview.file_too_big", previewProblem(PreviewKind.FILE, null, PREVIEW_MAX_BYTES + 1, null, false))
        assertNull(previewProblem(PreviewKind.PHOTO, "image/png", 10, null, false))
    }

    @Test
    fun `several items go as several messages in order with the caption on the first`() {
        val items = (1L..3L).map { PreviewItem(it, "f$it", null, ByteArray(0), null, PreviewKind.PHOTO) }
        val order = previewSendOrder(items, "  hi ")
        assertEquals(listOf(1L, 2L, 3L), order.map { it.first.id })
        assertEquals(listOf("hi", "", ""), order.map { it.second })
    }

    @Test
    fun `the audio clock reads minutes and hours`() {
        assertEquals("0:00", formatAudioClock(0))
        assertEquals("3:05", formatAudioClock(185_000))
        assertEquals("1:02:03", formatAudioClock(3_723_000))
    }

    @Test
    fun `bold on a selection makes runs and toggles back`() {
        val on = toggleSpan(emptyList(), 2 until 5)
        assertEquals(listOf(2..4), on)
        val block = EditBlock(EditKind.TEXT, "abcdefg", boldSpans = on)
        val runs = runsOf(block).map { it as ArticleRun.Text }
        assertEquals(listOf("ab", "cde", "fg"), runs.map { it.text })
        assertEquals(listOf(false, true, false), runs.map { it.bold })
        assertTrue(toggleSpan(on, 2 until 5).isEmpty())
    }

    @Test
    fun `spans follow an edit before them and drop deleted characters`() {
        // "abcdefg" bold c..e; insert "XY" at 0 → bold moves by two.
        assertEquals(listOf(4..6), shiftSpans("abcdefg", "XYabcdefg", listOf(2..4)))
        // delete "cd" → only "e" stays bold, now at 2.
        assertEquals(listOf(2..2), shiftSpans("abcdefg", "abefg", listOf(2..4)))
    }

    @Test
    fun `Send as file lifts the video rule and removing the last item closes the preview`() {
        val bad = PreviewItem(1, "v.mov", "video/quicktime", ByteArray(10), null, PreviewKind.VIDEO, "attach.preview.video_refused", "video/hevc")
        val unreadable = PreviewItem(2, "p", null, ByteArray(0), null, PreviewKind.FILE, "attach.preview.photo_unreadable")
        val p = SendPreview(listOf(bad, unreadable))
        val asFile = p.withSendAsFile(true)
        assertNull(asFile.items[0].problemKey)
        assertEquals("attach.preview.photo_unreadable", asFile.items[1].problemKey)
        assertEquals("attach.preview.video_refused", asFile.withSendAsFile(false).items[0].problemKey)
        assertEquals(listOf(2L), p.without(1)!!.items.map { it.id })
        assertNull(p.without(1)!!.without(2))
    }
}
