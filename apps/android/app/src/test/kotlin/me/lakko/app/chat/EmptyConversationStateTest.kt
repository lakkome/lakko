// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * docs/ux/navigation.md section 4, "Also with an empty list" — the empty state
 * shows only while the list is empty AND nothing is typed; a query on an empty
 * list reaches the search (the people half) instead.
 */
class EmptyConversationStateTest {
    @Test
    fun `an empty list with nothing typed shows the empty state`() {
        assertTrue(showsEmptyConversationState(listIsEmpty = true, query = ""))
    }

    @Test
    fun `whitespace alone counts as nothing typed`() {
        assertTrue(showsEmptyConversationState(listIsEmpty = true, query = "   "))
    }

    @Test
    fun `an empty list with a query gives way to the search`() {
        assertFalse(showsEmptyConversationState(listIsEmpty = true, query = "anna"))
    }

    @Test
    fun `a list with conversations never shows the empty state`() {
        assertFalse(showsEmptyConversationState(listIsEmpty = false, query = ""))
        assertFalse(showsEmptyConversationState(listIsEmpty = false, query = "anna"))
    }
}
