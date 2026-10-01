// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.GroupKind

/**
 * ADR-0060 — the list kind is read from the group's GroupContext, never
 * derived from the people count. The kind itself (and the one-to-one
 * membership rule) is asserted in Rust (`crates/lakko-mls/tests/one_to_one.rs`);
 * this host suite has no native library, so it covers the client mapping.
 */
class ConversationKindTest {

    @Test
    fun `a one-to-one is a chat`() {
        assertEquals(ConversationKind.Chat, conversationKindOf(GroupKind.ONE_TO_ONE))
    }

    @Test
    fun `a two-person group is a group`() {
        // The "Dev club" defect: a two-person group read as a one-to-one on
        // every device but its creator's. The people count is not an input.
        assertEquals(ConversationKind.Group, conversationKindOf(GroupKind.GROUP))
    }

    @Test
    fun `a group with no readable kind counts as a group`() {
        // Every conversation carries a kind; `null` covers non-conversation
        // objects and failed reads, which never get the one-to-one standing.
        assertEquals(ConversationKind.Group, conversationKindOf(null))
    }

    @Test
    fun `a group name is dropped in a one-to-one and applied elsewhere`() {
        assertFalse(groupNameAppliesTo(GroupKind.ONE_TO_ONE))
        assertTrue(groupNameAppliesTo(GroupKind.GROUP))
        assertTrue(groupNameAppliesTo(null))
    }
}
