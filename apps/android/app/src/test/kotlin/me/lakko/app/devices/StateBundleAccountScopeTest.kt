// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import me.lakko.app.backup.BackupRestoreStore
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * The pairing "known group ids" drive the one "Synchronizing..." label, and
 * they belong to the account the pairing was for (2026-09-30: they were
 * device-wide, so a second account showed the first account's
 * "Synchronizing...").
 *
 * They are the ONLY thing behind the label (2026-10-02): an account restored
 * from the phrase with an empty chat list shows none, and the group ids a
 * restored backup named never feed it.
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
    fun `a pending pairing-bundle group shows the label until it is joined`() {
        val context = RuntimeEnvironment.getApplication()
        StateBundle.saveKnownGroupIds(context, a, listOf("aa", "bb"))
        assertTrue(StateBundle.showsSynchronizing(context, a, emptyList()))
        assertTrue(StateBundle.showsSynchronizing(context, a, listOf("aa")))
        assertFalse(StateBundle.showsSynchronizing(context, a, listOf("aa", "bb")))
        // And it stays off: the stored list cleared itself.
        assertFalse(StateBundle.showsSynchronizing(context, a, emptyList()))
        assertFalse(StateBundle.showsSynchronizing(context, b, emptyList()))
        assertFalse(StateBundle.showsSynchronizing(context, null, emptyList()))
    }

    @Test
    fun `a restored account with an empty chat list shows no label`() {
        val context = RuntimeEnvironment.getApplication()
        // What a restore from the phrase leaves behind: the conversations the
        // backup named, none of them joined. That is not a pairing bundle.
        BackupRestoreStore.addRestoredGroups(context, a, listOf("aa".repeat(32), "bb".repeat(32)), emptyList())
        assertEquals(2, BackupRestoreStore.restoredGroups(context, a).size)

        assertFalse(StateBundle.showsSynchronizing(context, a, emptyList()))
        assertEquals(0, StateBundle.pendingKnownGroups(context, a, emptyList()))
    }

    @Test
    fun `a row the retired restored marker left behind shows no label`() {
        val context = RuntimeEnvironment.getApplication()
        // The marker an earlier build wrote for "landed through a recovery
        // route and holds no chat yet". Nothing reads it any more.
        context.getSharedPreferences("lakko-pairing-known-groups", android.content.Context.MODE_PRIVATE)
            .edit().putBoolean("restored.v2:$a", true).commit()

        assertFalse(StateBundle.showsSynchronizing(context, a, emptyList()))

        StateBundle.forgetAccount(context, a)
        assertFalse(
            context.getSharedPreferences("lakko-pairing-known-groups", android.content.Context.MODE_PRIVATE)
                .contains("restored.v2:$a"),
        )
    }

    @Test
    fun `forgetting one account keeps the other's`() {
        val context = RuntimeEnvironment.getApplication()
        StateBundle.saveKnownGroupIds(context, a, listOf("aa"))
        StateBundle.saveKnownGroupIds(context, b, listOf("bb"))

        StateBundle.forgetAccount(context, a)

        assertEquals(0, StateBundle.pendingKnownGroups(context, a, emptyList()))
        assertEquals(1, StateBundle.pendingKnownGroups(context, b, emptyList()))
        assertTrue(StateBundle.showsSynchronizing(context, b, emptyList()))
    }
}
