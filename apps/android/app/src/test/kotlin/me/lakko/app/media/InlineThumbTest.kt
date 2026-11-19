// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import me.lakko.app.chat.RowThumbSource
import me.lakko.app.chat.previewThumbOf
import me.lakko.app.push.PushPreview
import me.lakko.app.push.notificationPictureBytes
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.ThumbRef

/**
 * ADR-0047 section 6 — the micro thumbnail: the sender's size bound, the
 * notification picture decision, and the list row's preference for it.
 * Host JVM only: no Bitmap, no native core (the allowlist is injected).
 */
class InlineThumbTest {

    private fun jpeg(size: Int): ByteArray = ByteArray(size).also {
        it[0] = 0xFF.toByte()
        it[1] = 0xD8.toByte()
        it[2] = 0xFF.toByte()
    }

    private val allowlist: MediaAllowlist = { mime ->
        when (mime.substringBefore(';').trim().lowercase()) {
            "image/jpeg", "image/png", "image/gif", "image/webp" -> InlineKind.IMAGE
            "video/mp4" -> InlineKind.VIDEO
            "audio/mp4" -> InlineKind.AUDIO
            else -> null
        }
    }

    private fun ref(mime: String?, inline: ByteArray? = null, thumb: ThumbRef? = null, tag: Int = 0) = AttachmentRef(
        ByteArray(32) { tag.toByte() }, ByteArray(32), ByteArray(12), mime, 10u, null,
        thumb = thumb, inlineThumb = inline,
    )

    private fun thumb() = ThumbRef(ByteArray(32) { 9 }, ByteArray(32), ByteArray(12), "image/jpeg", 100u, 320u, 240u)

    // --- the bound every reader checks again -------------------------------

    @Test
    fun `a JPEG within 4096 bytes passes and anything else is refused`() {
        assertTrue(isInlineThumbJpeg(jpeg(3)))
        assertTrue(isInlineThumbJpeg(jpeg(INLINE_THUMB_MAX_BYTES)))
        assertFalse(isInlineThumbJpeg(jpeg(INLINE_THUMB_MAX_BYTES + 1)))
        assertFalse(isInlineThumbJpeg(null))
        assertFalse(isInlineThumbJpeg(ByteArray(0)))
        assertFalse(isInlineThumbJpeg(byteArrayOf(0xFF.toByte(), 0xD8.toByte())))
        // A PNG signature.
        assertFalse(isInlineThumbJpeg(byteArrayOf(0x89.toByte(), 0x50, 0x4E, 0x47, 0, 0)))
        assertFalse(isInlineThumbJpeg(byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0x00, 0x00)))
        assertEquals(96, INLINE_THUMB_LONGEST_SIDE)
    }

    // --- the sender's size bound ------------------------------------------

    @Test
    fun `the sender lowers the quality until the thumbnail fits`() {
        val tried = mutableListOf<Int>()
        val out = inlineThumbUnderBound(qualities = listOf(80, 60, 40, 20)) { q ->
            tried += q
            jpeg(if (q > 40) 6000 else 3000)
        }
        assertEquals(listOf(80, 60, 40), tried)
        assertEquals(3000, out?.size)
    }

    @Test
    fun `the sender gives up when no quality fits`() {
        val out = inlineThumbUnderBound(qualities = listOf(80, 40, 10)) { jpeg(INLINE_THUMB_MAX_BYTES + 1) }
        assertNull(out)
    }

    @Test
    fun `an encode that throws, is empty or is not a JPEG counts as no result`() {
        val out = inlineThumbUnderBound(qualities = listOf(90, 70, 50, 30)) { q ->
            when (q) {
                90 -> throw IllegalStateException("encoder")
                70 -> ByteArray(0)
                50 -> ByteArray(100)
                else -> jpeg(200)
            }
        }
        assertEquals(200, out?.size)
        assertTrue(INLINE_THUMB_QUALITY_LADDER.zipWithNext().all { (a, b) -> a > b })
    }

    @Test
    fun `the thumbnail goes out on the first photo or video only`() {
        val a = jpeg(100)
        val b = jpeg(200)
        val out = keepInlineThumbOnCarrier(
            listOf(ref("application/pdf", tag = 1), ref("image/jpeg", a, tag = 2), ref("video/mp4", b, tag = 3)),
            allowlist,
        )
        assertNull(out[0].inlineThumb)
        assertArrayEquals(a, out[1].inlineThumb)
        assertNull(out[2].inlineThumb)
    }

    @Test
    fun `a carrier without one sends none, and an out-of-bound one is dropped`() {
        // The first allowlisted photo carries nothing: the later one's goes too.
        val none = keepInlineThumbOnCarrier(listOf(ref("image/png", tag = 1), ref("image/jpeg", jpeg(10), tag = 2)), allowlist)
        assertTrue(none.all { it.inlineThumb == null })
        val over = keepInlineThumbOnCarrier(listOf(ref("image/jpeg", jpeg(INLINE_THUMB_MAX_BYTES + 1))), allowlist)
        assertNull(over.single().inlineThumb)
        // No thumbnail anywhere: the list is handed back untouched.
        val plain = listOf(ref("image/jpeg"))
        assertSame(plain, keepInlineThumbOnCarrier(plain, allowlist))
    }

    @Test
    fun `a file-picked JPEG first carries the thumbnail and the photo after it none`() {
        val fileJpeg = jpeg(120)
        val photo = jpeg(220)
        val tray = me.lakko.app.ui.chat.ComposerTray(
            items = listOf(
                me.lakko.app.ui.chat.TrayItem(
                    id = 1, kind = me.lakko.app.ui.chat.TrayKind.FILE, name = "scan.jpg",
                    status = me.lakko.app.ui.chat.TrayStatus.Ready, ref = ref("image/jpeg", fileJpeg, tag = 1),
                ),
                me.lakko.app.ui.chat.TrayItem(
                    id = 2, kind = me.lakko.app.ui.chat.TrayKind.PHOTO, name = null,
                    status = me.lakko.app.ui.chat.TrayStatus.Ready, ref = ref("image/jpeg", photo, tag = 2),
                ),
            ),
        )
        val out = tray.refs(allowlist)
        assertArrayEquals(fileJpeg, out[0].inlineThumb)
        assertNull(out[1].inlineThumb)
    }

    @Test
    fun `an item gets a thumbnail by its MIME, however it was added`() {
        assertTrue(me.lakko.app.ui.chat.wantsInlineThumb("image/jpeg", allowlist))
        assertTrue(me.lakko.app.ui.chat.wantsInlineThumb("video/mp4", allowlist))
        assertTrue(me.lakko.app.ui.chat.wantsInlineThumb("image/png; charset=binary", allowlist))
        assertFalse(me.lakko.app.ui.chat.wantsInlineThumb("application/pdf", allowlist))
        assertFalse(me.lakko.app.ui.chat.wantsInlineThumb("audio/mp4", allowlist))
        assertFalse(me.lakko.app.ui.chat.wantsInlineThumb(null, allowlist))
    }

    // --- the notification picture -----------------------------------------

    @Test
    fun `the notification has a picture only with its text`() {
        val pic = jpeg(500)
        assertArrayEquals(pic, notificationPictureBytes(PushPreview("Anna", "Photo", "tag", picture = pic)))
        assertNull(notificationPictureBytes(null))
        assertNull(notificationPictureBytes(PushPreview("Anna", "", "tag", picture = pic)))
        assertNull(notificationPictureBytes(PushPreview("Anna", "Photo", "tag")))
    }

    @Test
    fun `the notification refuses an over-bound or non-JPEG picture`() {
        assertNull(notificationPictureBytes(PushPreview("Anna", "Photo", "tag", picture = jpeg(INLINE_THUMB_MAX_BYTES + 1))))
        assertNull(notificationPictureBytes(PushPreview("Anna", "Photo", "tag", picture = ByteArray(500) { 0x47 })))
    }

    // --- the list row -----------------------------------------------------

    @Test
    fun `the list row draws the micro thumbnail first, with no fetch`() {
        val pic = jpeg(300)
        val row = previewThumbOf(listOf(ref("image/jpeg", pic, thumb = thumb())))!!
        val source = row.source
        assertTrue(source is RowThumbSource.Inline)
        assertArrayEquals(pic, (source as RowThumbSource.Inline).bytes)
    }

    @Test
    fun `the list row falls back to the sent thumb only without a micro thumbnail`() {
        val t = thumb()
        val blob = previewThumbOf(listOf(ref("image/jpeg", thumb = t)))!!.source
        assertTrue(blob is RowThumbSource.Blob)
        assertSame(t, (blob as RowThumbSource.Blob).thumb)
        // A micro thumbnail that fails the bound is no micro thumbnail.
        val bad = previewThumbOf(listOf(ref("image/jpeg", ByteArray(64), thumb = t)))!!.source
        assertTrue(bad is RowThumbSource.Blob)
        assertSame(RowThumbSource.None, previewThumbOf(listOf(ref("image/jpeg")))!!.source)
    }

    @Test
    fun `the list row reads the micro thumbnail wherever the core kept it`() {
        val pic = jpeg(50)
        // A first image the allowlist does not show, then the carrier.
        val row = previewThumbOf(listOf(ref("image/heic", tag = 1), ref("image/jpeg", pic, tag = 2)))!!
        assertEquals(2, row.count)
        assertArrayEquals(pic, row.inline)
    }
}
