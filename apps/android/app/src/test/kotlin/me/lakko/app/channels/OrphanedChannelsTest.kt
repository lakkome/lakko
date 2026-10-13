// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0073 phase 2 / UX 16.6 — the channels a key reset orphaned on the
 * device that reset the keys: which ones the record reads off the publisher
 * store, and that their keys are never re-opened.
 */
class OrphanedChannelsTest {

    private fun handle(id: String) = ChannelHandle(id = id, localName = "", localDescription = "", addedAtMs = 1uL)

    @Test
    fun `the owned channels are the ones with a persisted publisher signing seed`() {
        val raw = """[{"channelId":"aa","signingSeedHex":"00"},{"channelId":"bb","signingSeedHex":"01"},{"channelId":""}]"""
        assertEquals(setOf("aa", "bb"), OrphanedChannelsStore.publisherChannelIdsOf(raw))
        assertEquals(emptySet<String>(), OrphanedChannelsStore.publisherChannelIdsOf("not json"))
        assertEquals(emptySet<String>(), OrphanedChannelsStore.publisherChannelIdsOf("[]"))
    }

    @Test
    fun `an orphaned channel's publisher keys are never re-opened`() {
        val candidates = listOf(handle("aa"), handle("bb"), handle("cc"))
        assertEquals(
            listOf(handle("aa"), handle("cc")),
            channelsToReopenExcludingOrphaned(candidates, orphaned = setOf("bb")),
        )
        assertEquals(candidates, channelsToReopenExcludingOrphaned(candidates, orphaned = emptySet()))
    }

    @Test
    fun `the detail asks the set`() {
        assertTrue(channelIsOrphaned("bb", setOf("bb")))
        assertFalse(channelIsOrphaned("aa", setOf("bb")))
    }
}
