// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.ConversationKind as SharedConversationKind
import uniffi.lakko_ffi_uniffi.ConversationRow as SharedConversationRow

/**
 * docs/ux/navigation.md section 2 — the one conversation list.
 *
 * What is tested here is the MAPPING, not the order: the order is
 * `order_conversations` in `crates/lakko-ui-rules` and is tested there, in one
 * place, for all three clients. What this side owns is that the rich row and
 * the six fields the rule reads correspond, and that the indices the rule hands
 * back are applied to the rows they name. A fake orderer stands in for the
 * native call so the mapping runs on the host JVM.
 */
class ConversationListRowsTest {

    /** The fake: newest first, and it records what it was handed. */
    private class Orderer {
        var seen: List<SharedConversationRow> = emptyList()

        fun order(rows: List<SharedConversationRow>): List<UInt> {
            seen = rows
            return rows.indices
                .sortedByDescending { rows[it].lastActivityMs ?: Long.MIN_VALUE }
                .map { it.toUInt() }
        }
    }

    private fun row(
        id: String,
        kind: SharedConversationKind = SharedConversationKind.CHAT,
        title: String = id,
        at: Long? = null,
        pinned: Boolean = false,
        archived: Boolean = false,
    ) = ConversationListRow(
        id = id,
        kind = kind,
        title = title,
        preview = "",
        time = null,
        lastActivityMs = at,
        pinned = pinned,
        archived = archived,
    )

    @Test
    fun `the six fields the shared rule reads are the ones handed to it`() {
        val orderer = Orderer()
        val source = row(
            id = "abc",
            kind = SharedConversationKind.CHANNEL,
            title = "News",
            at = 42L,
            pinned = true,
            archived = true,
        )
        orderConversationRows(listOf(source), orderer::order)
        val sent = orderer.seen.single()
        assertEquals("abc", sent.id)
        assertEquals(SharedConversationKind.CHANNEL, sent.kind)
        assertEquals("News", sent.title)
        assertEquals(42L, sent.lastActivityMs)
        assertTrue(sent.pinned)
        assertTrue(sent.archived)
    }

    @Test
    fun `the app kind maps onto the shared kind a tap is routed by`() {
        assertEquals(SharedConversationKind.CHAT, ConversationKind.Chat.toRowKind())
        assertEquals(SharedConversationKind.GROUP, ConversationKind.Group.toRowKind())
    }

    @Test
    fun `the indices the rule returns are applied to the rows they name`() {
        val orderer = Orderer()
        val rows = listOf(row("a", at = 1L), row("b", at = 3L), row("c", at = 2L))
        val ordered = orderConversationRows(rows, orderer::order)
        assertEquals(listOf("b", "c", "a"), ordered.map { it.id })
    }

    @Test
    fun `a chat a group and a channel are one list rather than three`() {
        // The point of merging: the newest thing that happened does not care
        // which container it arrived in.
        val orderer = Orderer()
        val rows = listOf(
            row("chat", SharedConversationKind.CHAT, at = 10L),
            row("channel", SharedConversationKind.CHANNEL, at = 30L),
            row("group", SharedConversationKind.GROUP, at = 20L),
        )
        assertEquals(
            listOf("channel", "group", "chat"),
            orderConversationRows(rows, orderer::order).map { it.id },
        )
    }

    @Test
    fun `an empty list never reaches the rule at all`() {
        val orderer = Orderer()
        assertTrue(orderConversationRows(emptyList(), orderer::order).isEmpty())
        assertTrue(orderer.seen.isEmpty())
    }

    @Test
    fun `an index the rule could not name drops its row rather than throwing`() {
        // It cannot happen, but a `UInt` from across an FFI boundary is still
        // an input, and losing one row beats crashing the screen the app opens
        // on.
        val rows = listOf(row("a"), row("b"))
        val ordered = orderConversationRows(rows) { listOf(1u, 99u, 0u) }
        assertEquals(listOf("b", "a"), ordered.map { it.id })
    }

    @Test
    fun `a channel row carries none of the flags it has no id to key them on`() {
        val channel = row("chan", SharedConversationKind.CHANNEL, at = 1L)
        assertEquals(0, channel.unread)
        assertFalse(channel.pinned)
        assertFalse(channel.muted)
        assertFalse(channel.archived)
    }

    @Test
    fun `search matches a title or a preview, whatever the case`() {
        assertTrue(conversationRowMatchesQuery("ann", "Anna", "hello"))
        assertTrue(conversationRowMatchesQuery("HEL", "Anna", "hello"))
        assertFalse(conversationRowMatchesQuery("boris", "Anna", "hello"))
    }

    @Test
    fun `an empty or blank query matches everything`() {
        assertTrue(conversationRowMatchesQuery("", "Anna", "hello"))
        assertTrue(conversationRowMatchesQuery("   ", "Anna", "hello"))
    }
}
