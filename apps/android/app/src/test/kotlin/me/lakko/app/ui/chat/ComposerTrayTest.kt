// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.AttachmentRef

/** docs/ux/media-messages.md section 11 — the composer tray's rules. */
class ComposerTrayTest {

    private fun ref(n: Int) = AttachmentRef(ByteArray(32) { n.toByte() }, ByteArray(32), ByteArray(12), "image/jpeg", 10u, null)

    private fun item(id: Long, kind: TrayKind, status: TrayStatus = TrayStatus.Ready) =
        TrayItem(id, kind, "n$id", status = status, ref = if (status == TrayStatus.Ready) ref(id.toInt()) else null)

    @Test
    fun `adding past the limits keeps what fits and names the limit`() {
        val nine = ComposerTray(List(9) { item(it.toLong(), TrayKind.PHOTO) })
        val a = nine.admit(listOf(TrayKind.VIDEO, TrayKind.PHOTO, TrayKind.FILE))
        assertEquals(listOf(0, 2), a.admitted)
        assertEquals(TRAY_LIMIT_MEDIA_KEY, a.noticeKey)

        val tenFiles = ComposerTray(List(10) { item(it.toLong(), TrayKind.FILE) })
        val b = tenFiles.admit(listOf(TrayKind.FILE, TrayKind.PHOTO))
        assertEquals(listOf(1), b.admitted)
        assertEquals(TRAY_LIMIT_FILES_KEY, b.noticeKey)

        val c = ComposerTray().admit(List(12) { TrayKind.PHOTO })
        assertEquals((0 until 10).toList(), c.admitted)
        assertEquals(TRAY_LIMIT_MEDIA_KEY, c.noticeKey)

        val d = ComposerTray().admit(listOf(TrayKind.PHOTO, TrayKind.FILE))
        assertEquals(listOf(0, 1), d.admitted)
        assertNull(d.noticeKey)
    }

    @Test
    fun `send needs text or an item and every item uploaded`() {
        assertFalse(ComposerTray().canSend("  "))
        assertTrue(ComposerTray().canSend("hi"))
        val ready = ComposerTray(listOf(item(1, TrayKind.PHOTO), item(2, TrayKind.FILE)))
        assertTrue(ready.canSend(""))
        assertFalse(ready.busy)
        val uploading = ready.copy(items = ready.items + item(3, TrayKind.VIDEO, TrayStatus.Uploading(0.5f)))
        assertFalse(uploading.canSend("text"))
        val failed = ready.copy(items = ready.items + item(3, TrayKind.VIDEO, TrayStatus.Failed))
        assertFalse(failed.canSend("text"))
        assertTrue(failed.without(3).canSend(""))
        assertFalse(ComposerTray(listOf(item(4, TrayKind.PHOTO, TrayStatus.Preparing))).canSend("x"))
    }

    @Test
    fun `refs go in the order the items were added, media apart for the viewer`() {
        val t = ComposerTray(listOf(item(3, TrayKind.PHOTO), item(1, TrayKind.FILE), item(2, TrayKind.VIDEO)))
        assertEquals(listOf(3, 1, 2), t.refs().map { it.blobId[0].toInt() })
        assertEquals(listOf(3L, 2L), t.media().map { it.id })
        assertEquals(2, t.mediaCount)
        assertEquals(1, t.fileCount)
        val updated = t.update(1) { it.copy(status = TrayStatus.Failed, ref = null) }
        assertEquals(TrayStatus.Failed, updated.item(1)?.status)
        assertEquals(2, updated.refs().size)
    }

    @Test
    fun `the limit line shows for four seconds`() {
        val t = ComposerTray(notice = TrayNotice(UiText(TRAY_LIMIT_MEDIA_KEY), atMs = 1_000))
        assertEquals(TRAY_LIMIT_MEDIA_KEY, t.noticeAt(1_000)?.key)
        assertEquals(TRAY_LIMIT_MEDIA_KEY, t.noticeAt(4_999)?.key)
        assertNull(t.noticeAt(5_000))
        assertNull(ComposerTray().noticeAt(0))
    }

    @Test
    fun `an item is taken by its type, and what cannot go as media goes as a file`() {
        assertEquals(TrayKind.PHOTO, trayKindOf("image/heic", filesOnly = false))
        assertEquals(TrayKind.VIDEO, trayKindOf("video/quicktime", filesOnly = false))
        assertEquals(TrayKind.FILE, trayKindOf("application/pdf", filesOnly = false))
        assertEquals(TrayKind.FILE, trayKindOf(null, filesOnly = false))
        assertEquals(TrayKind.FILE, trayKindOf("image/png", filesOnly = true))

        assertEquals(TrayKind.PHOTO, trayKindAfterRead(TrayKind.PHOTO, "image/heic", decodedPhoto = true, videoCodec = null))
        assertEquals(TrayKind.FILE, trayKindAfterRead(TrayKind.PHOTO, "image/png", decodedPhoto = false, videoCodec = null))
        assertEquals(TrayKind.VIDEO, trayKindAfterRead(TrayKind.VIDEO, "video/mp4", false, "video/avc"))
        assertEquals(TrayKind.FILE, trayKindAfterRead(TrayKind.VIDEO, "video/mp4", false, "video/hevc"))
        assertEquals(TrayKind.FILE, trayKindAfterRead(TrayKind.VIDEO, "video/quicktime", false, "video/avc"))
        assertEquals(TrayKind.FILE, trayKindAfterRead(TrayKind.FILE, "image/png", true, null))
    }
}
