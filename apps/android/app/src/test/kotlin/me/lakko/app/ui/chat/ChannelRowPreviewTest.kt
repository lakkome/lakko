// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.feed.ChannelPostSlot
import me.lakko.app.feed.PersistedChannelPost
import me.lakko.app.feed.PersistedChannelPostMedia
import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * docs/ux/navigation.md section 2 — a channel row's second line is its newest
 * post the device can read; the @name only when there is none.
 */
class ChannelRowPreviewTest {

    init {
        EnglishForTests.install()
    }

    @Test
    fun `the newest post is the line, its first non-blank line`() {
        val slot = ChannelPostSlot(
            cursor = 3,
            posts = listOf(
                PersistedChannelPost(1, 1_000, "first"),
                PersistedChannelPost(3, 3_000, "\n  newest post\nsecond line"),
                PersistedChannelPost(2, 2_000, "middle"),
            ),
        )
        val last = channelLastPost(slot)
        assertEquals("newest post", last?.text)
        assertEquals(3_000L, last?.serverTsMs)
    }

    @Test
    fun `a newest post with only attachments is named as a group row names it`() {
        val slot = ChannelPostSlot(
            cursor = 4,
            posts = listOf(
                PersistedChannelPost(1, 1_000, "First post in Walk 22 channel"),
                PersistedChannelPost(2, 2_000, "", listOf(PersistedChannelPostMedia("image/jpeg", null))),
                PersistedChannelPost(3, 3_000, "", listOf(PersistedChannelPostMedia("text/plain", null))),
                PersistedChannelPost(4, 4_000, "", listOf(PersistedChannelPostMedia("image/gif", "gif"))),
            ),
        )
        val last = channelLastPost(slot)!!
        assertEquals(4_000L, last.serverTsMs)
        assertEquals("GIF", channelRowPreviewLine(last))
        val photo = PersistedChannelPost(2, 2_000, "", listOf(PersistedChannelPostMedia("image/jpeg", null)))
        assertEquals("Photo", channelRowPreviewLine(photo))
        assertEquals("File", channelRowPreviewLine(PersistedChannelPost(3, 3_000, "", listOf(PersistedChannelPostMedia("text/plain", null)))))
        assertEquals("Voice message", channelRowPreviewLine(PersistedChannelPost(5, 5_000, "", listOf(PersistedChannelPostMedia("audio/mp4", "voice")))))
        assertEquals("Video message", channelRowPreviewLine(PersistedChannelPost(6, 6_000, "", listOf(PersistedChannelPostMedia("video/mp4", "video_note")))))
        assertEquals("caption", channelRowPreviewLine(PersistedChannelPost(7, 7_000, "caption", listOf(PersistedChannelPostMedia("image/png", null)))))
    }

    @Test
    fun `no readable post leaves the row to its handle`() {
        assertNull(channelLastPost(null))
        assertNull(channelLastPost(ChannelPostSlot()))
        assertNull(channelLastPost(ChannelPostSlot(posts = listOf(PersistedChannelPost(4, 4_000, "  ")))))
    }
}
