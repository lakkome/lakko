// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Mark all read in Chats reads the tab's channels too (follow-up to base
 * scenario 29, 2026-11-13): each is read through the newest post this device
 * holds, so its count clears; a marker never moves backwards.
 */
class ChannelMarkAllReadTest {
    private val a = "aa".repeat(16)
    private val b = "bb".repeat(16)

    @Test
    fun `each channel is read through its newest held post`() {
        val r = FeedReading(channelRead = mapOf(a to 3L, b to 9L))
        val slots = mapOf(
            a to ChannelPostSlot(cursor = 4L, posts = listOf(PersistedChannelPost(5, 1, "x"), PersistedChannelPost(7, 2, "y"))),
            b to ChannelPostSlot(cursor = 2L, posts = listOf(PersistedChannelPost(5, 1, "x"))),
        )
        val next = markChannelsRead(r, listOf(a, b), slots)
        assertEquals(7L, next.channelRead[a])
        // Never backwards.
        assertEquals(9L, next.channelRead[b])
        assertEquals(0, channelUnreadCount(listOf(5L, 7L), next.channelRead[a], own = false))
    }

    @Test
    fun `a channel with no slot is left as it is`() {
        val r = FeedReading(channelRead = mapOf(a to 3L))
        assertEquals(r, markChannelsRead(r, listOf(a), emptyMap()))
    }
}
