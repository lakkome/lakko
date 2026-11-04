// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0058 / ADR-0065 - a persisted synced-state blob the core refuses is
 * "no synced state held", in this device's state file and inside a restored
 * backup alike: reported once, never thrown, and replaced by what is written
 * next.
 *
 * The host JVM cannot load the native library, so [VersionGate] stands in
 * for `SyncedState.fromBytes`: it refuses every first byte but the current
 * encoding version, which is the rule of `SyncedState::from_bytes` in
 * `crates/lakko-sync/src/synced/state.rs` (`STATE_VERSION` = 2; version 1,
 * the layout before saved messages, is refused). What these tests pin is
 * this client's handling of that refusal, not the refusal itself, which the
 * core's own tests cover.
 */
class SyncedStateBlobTest {

    /** Refuses whatever does not open with the current version byte. */
    private class VersionGate : SyncedStateBytes {
        var merges = 0

        private fun check(bytes: ByteArray) {
            if (bytes.isEmpty() || bytes[0] != CURRENT) error("unknown synced-state version")
        }

        override fun canonical(bytes: ByteArray): ByteArray {
            check(bytes)
            return bytes.copyOf()
        }

        override fun merged(existing: ByteArray, backup: ByteArray): ByteArray {
            check(existing)
            check(backup)
            merges += 1
            // Stand-in fold: the existing body followed by the backup's.
            return existing + backup.copyOfRange(1, backup.size)
        }
    }

    /** A version 1 blob: first byte 0x01 followed by anything. */
    private val v1 = byteArrayOf(0x01, 0x00, 0x00, 0x00, 0x07, 0x55, 0x66)
    private val v2a = byteArrayOf(CURRENT, 0x0a)
    private val v2b = byteArrayOf(CURRENT, 0x0b)

    // ------------------------------------------------------------ the state file

    @Test
    fun aVersionOneFileIsNoneHeldAndReportedOnce() {
        val refusals = mutableListOf<Throwable>()
        val held = heldSyncedStateBytes(v1, VersionGate()) { refusals += it }
        assertNull(held)
        assertEquals(1, refusals.size)
    }

    @Test
    fun aCurrentFileIsHeld() {
        val refusals = mutableListOf<Throwable>()
        assertArrayEquals(v2a, heldSyncedStateBytes(v2a, VersionGate()) { refusals += it })
        assertTrue(refusals.isEmpty())
    }

    @Test
    fun noFileAndAnEmptyFileAreNoneHeldWithoutARefusal() {
        val refusals = mutableListOf<Throwable>()
        assertNull(heldSyncedStateBytes(null, VersionGate()) { refusals += it })
        assertNull(heldSyncedStateBytes(ByteArray(0), VersionGate()) { refusals += it })
        assertTrue(refusals.isEmpty())
    }

    // ------------------------------------------------------------ a restored backup

    @Test
    fun aVersionOneBackupSeedsNothingAndLeavesTheFileAlone() {
        val refused = mutableListOf<RefusedSyncedState>()
        val core = VersionGate()
        assertNull(backupSeedBytes(stored = v2a, backup = v1, core = core) { which, _ -> refused += which })
        assertEquals(listOf(RefusedSyncedState.BACKUP), refused)
        assertEquals(0, core.merges)
    }

    @Test
    fun aVersionOneFileIsReplacedByTheBackupsState() {
        val refused = mutableListOf<RefusedSyncedState>()
        val core = VersionGate()
        val seed = backupSeedBytes(stored = v1, backup = v2b, core = core) { which, _ -> refused += which }
        assertArrayEquals(v2b, seed)
        assertEquals(listOf(RefusedSyncedState.STORED), refused)
        assertEquals("nothing of the refused file is folded in", 0, core.merges)
    }

    @Test
    fun aVersionOneBackupOverAVersionOneFileWritesNothing() {
        val refused = mutableListOf<RefusedSyncedState>()
        assertNull(backupSeedBytes(stored = v1, backup = v1, core = VersionGate()) { which, _ -> refused += which })
        // The backup is read first; a refused backup ends the seed there.
        assertEquals(listOf(RefusedSyncedState.BACKUP), refused)
    }

    @Test
    fun aBackupOverNoFileSeedsTheBackupsState() {
        val refused = mutableListOf<RefusedSyncedState>()
        assertArrayEquals(
            v2b,
            backupSeedBytes(stored = null, backup = v2b, core = VersionGate()) { which, _ -> refused += which },
        )
        assertTrue(refused.isEmpty())
    }

    @Test
    fun aBackupOverACurrentFileIsFoldedIntoIt() {
        val refused = mutableListOf<RefusedSyncedState>()
        val core = VersionGate()
        val seed = backupSeedBytes(stored = v2a, backup = v2b, core = core) { which, _ -> refused += which }
        assertArrayEquals(byteArrayOf(CURRENT, 0x0a, 0x0b), seed)
        assertEquals(1, core.merges)
        assertTrue(refused.isEmpty())
    }

    @Test
    fun aFoldThatFailsWritesNothingAndDoesNotThrow() {
        val refused = mutableListOf<RefusedSyncedState>()
        val failing = object : SyncedStateBytes {
            override fun canonical(bytes: ByteArray): ByteArray = bytes.copyOf()
            override fun merged(existing: ByteArray, backup: ByteArray): ByteArray = error("fold refused")
        }
        assertNull(backupSeedBytes(stored = v2a, backup = v2b, core = failing) { which, _ -> refused += which })
        assertEquals(listOf(RefusedSyncedState.MERGE), refused)
    }

    private companion object {
        /** `STATE_VERSION` in `crates/lakko-sync/src/synced/state.rs`. */
        const val CURRENT: Byte = 0x02
    }

    /** ADR-0087 section 9 - a flag day: the state file of the version before
     *  does not stop a sign-in, the session starts clean. */
    @Test
    fun `a state file this build refuses starts clean and carries on`() {
        val gate = VersionGate()
        var refusals = 0
        val seeded = seedStateOrFresh(
            stored = byteArrayOf(0x06, 9),
            open = { gate.canonical(it) },
            fresh = { byteArrayOf(CURRENT) },
            onRefused = { refusals += 1 },
        )
        assertArrayEquals(byteArrayOf(CURRENT), seeded)
        assertEquals(1, refusals)
    }

    @Test
    fun `no state file is a fresh state, with nothing to report`() {
        var refusals = 0
        val seeded = seedStateOrFresh<ByteArray>(null, { error("nothing to open") }, { byteArrayOf(CURRENT) }, { refusals += 1 })
        assertArrayEquals(byteArrayOf(CURRENT), seeded)
        assertEquals(0, refusals)
    }

    @Test
    fun `a state file this build reads is opened`() {
        val gate = VersionGate()
        val seeded = seedStateOrFresh(byteArrayOf(CURRENT, 9), { gate.canonical(it) }, { ByteArray(0) }, {})
        assertArrayEquals(byteArrayOf(CURRENT, 9), seeded)
    }
}
