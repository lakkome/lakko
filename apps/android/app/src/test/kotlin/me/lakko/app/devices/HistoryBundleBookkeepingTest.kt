// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import me.lakko.app.chat.PersistedCall
import me.lakko.app.chat.PersistedMessage
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The MLS bookkeeping rows (a commit, an external-join request) never travel
 * in a history bundle, and a bundle that still carries them has them dropped
 * on the new device: a commit row names no people, and a receiver that draws
 * it showed "Members changed" once per device commit (seen live 2026-10-28).
 */
class HistoryBundleBookkeepingTest {

    private fun row(id: Long, direction: String, text: String, call: PersistedCall? = null) =
        PersistedMessage(id, direction, text, emptyList(), "2026-10-28T10:00:00Z", call = call)

    private val commit = row(2, "system", "[mls commit]")
    private val join = row(3, "system", "[mls external join]")
    private val callRow = row(4, "system", "", call = PersistedCall(outcome = "missed"))
    private val hello = row(1, "incoming", "hello")
    private val reply = row(5, "outgoing", "[mls commit]")

    @Test
    fun `the builder leaves the bookkeeping rows out`() {
        assertEquals(
            listOf(hello, callRow, reply),
            HistoryBundleCodec.bundledMessages(listOf(hello, commit, join, callRow, reply)),
        )
    }

    @Test
    fun `the receiver drops bookkeeping rows a sender still carries`() {
        val held = listOf(row(10, "incoming", "held"))
        val (merged, added) = HistoryBundleCodec.mergeRows(held, listOf(reply, commit, join, hello, callRow, hello))
        assertEquals(listOf(1L, 4L, 5L, 10L), merged.map { it.messageId })
        assertEquals(3, added)
    }
}
