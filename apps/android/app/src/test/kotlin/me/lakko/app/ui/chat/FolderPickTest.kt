// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.chat.ConversationListRow
import me.lakko.app.chat.ConversationRowKind
import me.lakko.app.chat.FolderDraft
import me.lakko.app.chat.rowIdOf
import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FolderConversationKind
import uniffi.lakko_ffi_uniffi.FolderConversationRef

/**
 * docs/ux/chat-folders.md 3.3 — the chat picker's result applied to the
 * editor draft. A chat this device cannot show is kept (ADR-0058 section 5a);
 * a chat moved into one hand list leaves the other.
 */
class FolderPickTest {

    private fun row(id: String) = ConversationListRow(
        id = id,
        kind = ConversationRowKind.GROUP,
        title = id,
        preview = "",
        time = null,
        lastActivityMs = null,
    )

    private fun ref(id: String) = FolderConversationRef(
        FolderConversationKind.GROUP,
        ByteArray(id.length / 2) { i -> id.substring(2 * i, 2 * i + 2).toInt(16).toByte() },
    )

    private val a = "aa".repeat(32)
    private val b = "bb".repeat(32)
    private val hidden = "dd".repeat(32)

    @Test
    fun unchecking_a_visible_chat_keeps_the_ones_this_device_cannot_show() {
        val draft = FolderDraft.empty().copy(included = listOf(ref(hidden), ref(a)))
        val next = applyPick(draft, FolderPickMode.Include, checkedIds = emptySet(), rows = listOf(row(a), row(b)))
        assertEquals(listOf(hidden), next.included.map(::rowIdOf))
    }

    @Test
    fun adding_to_included_takes_the_chat_out_of_excluded() {
        val draft = FolderDraft.empty().copy(excluded = listOf(ref(b)))
        val next = applyPick(draft, FolderPickMode.Include, checkedIds = setOf(a, b), rows = listOf(row(a), row(b)))
        assertEquals(setOf(a, b), next.included.map(::rowIdOf).toSet())
        assertEquals(emptyList<String>(), next.excluded.map(::rowIdOf))
    }

    @Test
    fun excluding_takes_the_chat_out_of_included() {
        val draft = FolderDraft.empty().copy(included = listOf(ref(a)))
        val next = applyPick(draft, FolderPickMode.Exclude, checkedIds = setOf(a), rows = listOf(row(a)))
        assertEquals(listOf(a), next.excluded.map(::rowIdOf))
        assertEquals(emptyList<String>(), next.included.map(::rowIdOf))
    }
}
