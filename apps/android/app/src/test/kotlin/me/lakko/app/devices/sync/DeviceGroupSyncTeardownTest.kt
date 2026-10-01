// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import android.content.Context
import java.io.File
import me.lakko.app.devices.DeviceGroupSnapshotStore
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * Log out keeps the account's synced state (ADR-0058: folders, flags, read
 * cursors, drafts, order) and its "a write is waiting" flag for the next
 * sign-in; only Forget device and Delete account, through the account wipe,
 * remove them. The restored DeviceGroup snapshot has already consumed the
 * journal rows that built this state, so a deleted copy is simply gone.
 *
 * The Keystore-wrapped envelope is opaque bytes on disk; a raw file at the
 * store's path stands in for it (Robolectric has no Android Keystore).
 */
@RunWith(RobolectricTestRunner::class)
class DeviceGroupSyncTeardownTest {

    private lateinit var context: Context

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        for (account in listOf(ACCOUNT, OTHER)) {
            stateFile(account).apply { parentFile?.mkdirs() }.writeText("state of $account")
            pendingPrefs().edit().putBoolean(account, true).commit()
        }
    }

    @Test
    fun log_out_keeps_the_synced_state_and_the_pending_flag() {
        val repository = DeviceGroupSyncRepository(context)
        repository.bindAccountForTest(ACCOUNT)

        repository.clear()
        repository.clear() // the session root runs it again on leaving the session

        assertEquals("state of $ACCOUNT", stateFile(ACCOUNT).readText())
        assertTrue(pendingPrefs().getBoolean(ACCOUNT, false))
        assertFalse(repository.pendingSync.value)
        repository.close()
    }

    @Test
    fun the_account_wipe_removes_them_and_only_them() {
        DeviceGroupSyncRepository.deleteAccountRows(context, ACCOUNT)

        assertFalse(stateFile(ACCOUNT).exists())
        assertFalse(pendingPrefs().contains(ACCOUNT))
        assertEquals("state of $OTHER", stateFile(OTHER).readText())
        assertTrue(pendingPrefs().getBoolean(OTHER, false))
    }

    private fun stateFile(account: String): File =
        File(
            File(context.noBackupFilesDir, DeviceGroupSnapshotStore.SYNC_STATE_DIR),
            "${account.replace('/', '_')}.bin",
        )

    private fun pendingPrefs() =
        context.getSharedPreferences(DeviceGroupSyncRepository.PENDING_PREFS, Context.MODE_PRIVATE)

    private companion object {
        const val ACCOUNT = "AAAAAAAAAAAAAAAAAAAAAA=="
        const val OTHER = "AQEBAQEBAQEBAQEBAQEBAQ=="
    }
}
