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
 * What is tested here is the MAPPING and the WIRING, not the order: the order is
 * `order_conversations` in `crates/lakko-ui-rules` and is tested there, in one
 * place, for all three clients. What this side owns is that the rich row and
 * the six fields the rule reads correspond, that the indices the rule hands
 * back are applied to the rows they name, and that nothing on this side decides
 * the order beside it. A fake orderer stands in for the native call so the
 * mapping runs on the host JVM.
 *
 * ## The screen-level gap, named rather than approximated
 *
 * These reach `orderConversationRows`, which is the seam `ChatListScreen` calls
 * with no orderer of its own. What they do NOT reach is the screen: a test that
 * composed `ChatListScreen` and read the rendered rows would be the assertion
 * that the LIST a person sees is in the rule's order, and it cannot run in
 * `src/test`. The screen reaches the native core three ways before it draws
 * anything — `orderConversations` itself, `ChatRepository.memberCount` behind
 * `conversationKind`, and `conversationTitle` — and this project builds
 * `liblakko_ffi_uniffi.so` only for Android ABIs, so there is no host-JVM copy
 * for Robolectric to load. Reaching it needs either an instrumented
 * (`androidTest`) suite on a device, or a host-target build of the FFI crate put
 * on `java.library.path`, which is a change to the test infrastructure rather
 * than a test.
 *
 * What is left on this side, and what the last two tests below are for, is the
 * part of the screen's pipeline that IS a pure composition of two public
 * functions: order by the rule, then filter by the query, and nothing else.
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

    /**
     * The order is the rule's answer VERBATIM: nothing on this side re-sorts it.
     *
     * Every other test here hands back an order that is also a sort of the data
     * — the fake orderer sorts by last activity — so a `sortedByDescending` that
     * crept in beside the rule would satisfy all of them. This one hands back a
     * permutation that NO comparator over the row's own fields produces: not by
     * time either way, not by title either way, and not pinned-first or
     * archived-last, because the archived row comes back in the middle.
     *
     * That is the shape of the defect worth guarding. The rule earns its place
     * by being one copy behind three clients; a second opinion on this side
     * would not announce itself, it would just quietly disagree with iOS.
     */
    @Test
    fun `the order is the rule's answer verbatim and nothing re-sorts it`() {
        val rows = listOf(
            row("a", title = "a", at = 1L, pinned = true),
            row("b", title = "b", at = 2L),
            row("c", title = "c", at = 3L, archived = true),
        )

        val ordered = orderConversationRows(rows) { listOf(0u, 2u, 1u) }

        assertEquals(listOf("a", "c", "b"), ordered.map { it.id })
    }

    /**
     * The screen's two steps, in the screen's order: ask the rule, then drop
     * what the query does not match.
     *
     * The filter is what makes this worth asserting rather than obvious — a
     * search that re-ranked by relevance would be a perfectly reasonable thing
     * to write and would take the pinned row out of first place while a person
     * was typing. `docs/ux/navigation.md` section 4 gives search one job, which
     * is to hide rows.
     */
    @Test
    fun `the query filter hides rows without reordering the ones it keeps`() {
        val rows = listOf(
            row("a", title = "Anna", at = 1L),
            row("b", title = "Boris", at = 2L),
            row("c", title = "Anton", at = 3L),
        )
        // The rule's answer, in an order the query has no reason to agree with.
        val ordered = orderConversationRows(rows) { listOf(2u, 1u, 0u) }

        val visible = ordered.filter { conversationRowMatchesQuery("an", it.title, it.preview) }

        // Boris is gone and the two that stayed kept the rule's relative order,
        // which put Anton before Anna.
        assertEquals(listOf("c", "a"), visible.map { it.id })
    }

    @Test
    fun `an empty or blank query matches everything`() {
        assertTrue(conversationRowMatchesQuery("", "Anna", "hello"))
        assertTrue(conversationRowMatchesQuery("   ", "Anna", "hello"))
    }
}
