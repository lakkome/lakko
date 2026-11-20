// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import android.net.Uri
import me.lakko.app.chat.PastedMedia
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/** docs/ux/attachments.md 2.5 in a channel: who a pasted photo or video attaches for. */
@RunWith(RobolectricTestRunner::class)
class ChannelPasteTest {

    private data class Opened(val uris: List<Uri>, val filesOnly: Boolean, val mimes: List<String?>)

    @Test
    fun aPublishersPasteOpensThePhotoOrVideoPreview() {
        val opened = mutableListOf<Opened>()
        val handler = channelPasteHandler(isPublisher = true) { uris, filesOnly, mimes ->
            opened += Opened(uris, filesOnly, mimes)
        }
        val pic = Uri.parse("content://me.lakko.test/pic")
        val clip = Uri.parse("content://me.lakko.test/clip")

        requireNotNull(handler)(listOf(PastedMedia(pic, "image/gif"), PastedMedia(clip, "video/mp4")))

        assertEquals(listOf(Opened(listOf(pic, clip), false, listOf("image/gif", "video/mp4"))), opened)
    }

    @Test
    fun anEmptyPasteOpensNothing() {
        val opened = mutableListOf<Opened>()
        val handler = channelPasteHandler(isPublisher = true) { uris, filesOnly, mimes ->
            opened += Opened(uris, filesOnly, mimes)
        }
        requireNotNull(handler)(emptyList())
        assertTrue(opened.isEmpty())
    }

    @Test
    fun aSubscriberHasNoMediaPaste() {
        var calls = 0
        val handler = channelPasteHandler(isPublisher = false) { _, _, _ -> calls += 1 }
        assertNull("a subscriber's paste must stay text", handler)
        assertEquals(0, calls)
    }
}
