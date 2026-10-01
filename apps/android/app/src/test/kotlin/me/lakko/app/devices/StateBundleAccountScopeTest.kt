// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * The pairing "known group ids" and the restored marker drive the one
 * "Synchronizing..." label; both belong to the account the pairing or
 * recovery was for (2026-09-30: they were device-wide, so a second account
 * showed the first account's "Synchronizing...").
 */
@RunWith(RobolectricTestRunner::class)
class StateBundleAccountScopeTest {

    private val a = "CgoKCgoKCgoKCgoKCgoKCg=="
    private val b = "CwsLCwsLCwsLCwsLCwsLCw=="

    @Test
    fun `known groups of one account are not pending for another`() {
        val context = RuntimeEnvironment.getApplication()
        StateBundle.saveKnownGroupIds(context, a, listOf("aa", "bb"))
        assertEquals(2, StateBundle.pendingKnownGroups(context, a, emptyList()))
        assertEquals(0, StateBundle.pendingKnownGroups(context, b, emptyList()))
        assertEquals(0, StateBundle.pendingKnownGroups(context, null, emptyList()))
        assertEquals(1, StateBundle.pendingKnownGroups(context, a, listOf("aa")))
    }

    @Test
    fun `the restored marker is per account`() {
        val context = RuntimeEnvironment.getApplication()
        StateBundle.markRestored(context, a)
        assertTrue(StateBundle.restoredAwaitingChats(context, a, joinedCount = 0))
        assertFalse(StateBundle.restoredAwaitingChats(context, b, joinedCount = 0))
        assertFalse(StateBundle.restoredAwaitingChats(context, null, joinedCount = 0))
        // The first chat clears it for that account only.
        StateBundle.markRestored(context, b)
        assertFalse(StateBundle.restoredAwaitingChats(context, a, joinedCount = 1))
        assertFalse(StateBundle.restoredAwaitingChats(context, a, joinedCount = 0))
        assertTrue(StateBundle.restoredAwaitingChats(context, b, joinedCount = 0))
    }

    @Test
    fun `forgetting one account keeps the other's`() {
        val context = RuntimeEnvironment.getApplication()
        StateBundle.saveKnownGroupIds(context, a, listOf("aa"))
        StateBundle.saveKnownGroupIds(context, b, listOf("bb"))
        StateBundle.markRestored(context, a)
        StateBundle.markRestored(context, b)

        StateBundle.forgetAccount(context, a)

        assertEquals(0, StateBundle.pendingKnownGroups(context, a, emptyList()))
        assertFalse(StateBundle.restoredAwaitingChats(context, a, joinedCount = 0))
        assertEquals(1, StateBundle.pendingKnownGroups(context, b, emptyList()))
        assertTrue(StateBundle.restoredAwaitingChats(context, b, joinedCount = 0))
    }
}
