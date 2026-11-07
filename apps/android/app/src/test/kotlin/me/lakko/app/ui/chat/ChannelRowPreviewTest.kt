// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.feed.ChannelPostSlot
import me.lakko.app.feed.PersistedChannelPost
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * docs/ux/navigation.md section 2 — a channel row's second line is its newest
 * post the device can read; the @name only when there is none.
 */
class ChannelRowPreviewTest {

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
    fun `no readable post leaves the row to its handle`() {
        assertNull(channelLastPost(null))
        assertNull(channelLastPost(ChannelPostSlot()))
        assertNull(channelLastPost(ChannelPostSlot(posts = listOf(PersistedChannelPost(4, 4_000, "  ")))))
    }
}
