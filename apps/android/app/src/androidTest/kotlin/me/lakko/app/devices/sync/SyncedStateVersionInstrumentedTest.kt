// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.SyncedState

/**
 * ADR-0058 / ADR-0065 - the native half of [SyncedStateBlobTest].
 *
 * The host suite pins this client's handling of a refused blob behind a
 * stand-in decoder. This suite runs against `liblakko_ffi_uniffi.so`, so it
 * pins the part the host cannot: that the real `SyncedState.fromBytes`
 * refuses an encoding version 1 blob (first byte 0x01), and that
 * [CoreSyncedStateBytes] turns that refusal into "none held".
 */
@RunWith(AndroidJUnit4::class)
class SyncedStateVersionInstrumentedTest {

    /** A version 1 blob: first byte 0x01 followed by anything. */
    private val v1 = byteArrayOf(0x01, 0x00, 0x00, 0x00, 0x00)

    private fun emptyStateBytes(): ByteArray = SyncedState().use { it.toBytes() }

    @Test
    fun theCoreWritesVersionTwo() {
        assertEquals(0x02.toByte(), emptyStateBytes()[0])
    }

    @Test
    fun theCoreRefusesAVersionOneBlob() {
        try {
            SyncedState.fromBytes(v1).close()
            fail("a version 1 blob was accepted")
        } catch (refused: FfiException.SyncedState) {
            // The refusal every reader below turns into "none held".
        }
    }

    @Test
    fun aVersionOneFileIsNoneHeld() {
        val refusals = mutableListOf<Throwable>()
        assertNull(heldSyncedStateBytes(v1, CoreSyncedStateBytes) { refusals += it })
        assertEquals(1, refusals.size)
    }

    @Test
    fun aCurrentFileIsHeld() {
        val current = emptyStateBytes()
        val refusals = mutableListOf<Throwable>()
        assertArrayEquals(current, heldSyncedStateBytes(current, CoreSyncedStateBytes) { refusals += it })
        assertTrue(refusals.isEmpty())
    }

    @Test
    fun aVersionOneFileIsReplacedByTheBackupsState() {
        val backup = emptyStateBytes()
        val refused = mutableListOf<RefusedSyncedState>()
        val seed = backupSeedBytes(v1, backup, CoreSyncedStateBytes) { which, _ -> refused += which }
        assertArrayEquals(backup, seed)
        assertEquals(listOf(RefusedSyncedState.STORED), refused)
    }

    @Test
    fun aVersionOneBackupSeedsNothing() {
        val refused = mutableListOf<RefusedSyncedState>()
        assertNull(backupSeedBytes(emptyStateBytes(), v1, CoreSyncedStateBytes) { which, _ -> refused += which })
        assertEquals(listOf(RefusedSyncedState.BACKUP), refused)
    }
}
