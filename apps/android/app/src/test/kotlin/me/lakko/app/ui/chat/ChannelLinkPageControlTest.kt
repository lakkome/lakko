// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * UX `group-and-channel-privacy.md` 14.1 / 7.2 — live walk 2026-11-24: an
 * already-admitted subscriber opened the channel's `/g/` link again and saw
 * `Subscribe`. The page shows `Open` for a channel this device follows.
 */
class ChannelLinkPageControlTest {
    private val channel = ByteArray(16) { 0x0C }
    private val hex = "0c".repeat(16)

    @Test
    fun aFollowedChannelOpens() {
        assertEquals(hex, followedChannelHex(channel) { it == hex })
    }

    @Test
    fun anUnfollowedChannelSubscribes() {
        assertNull(followedChannelHex(channel) { false })
        assertNull(followedChannelHex(null) { true })
    }
}
