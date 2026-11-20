// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.feed.PersistedChannelPost
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * A channel row stands in the list by its newest real post. Seen live
 * 2026-11-12: a ban list the owner posted at 22:48 stood for a channel whose
 * newest post was at 22:39. The raw server rows (ban lists, metadata
 * records) are not posts; the channel-post log holds posts only.
 */
class ChannelRowActivityTest {
    @Test
    fun `the newest real post decides, else when the channel was added`() {
        assertEquals(9_000L, channelRowActivityMs(PersistedChannelPost(5, 9_000, "post"), 1L))
        assertEquals(1L, channelRowActivityMs(null, 1L))
    }
}
