// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

/*
 * ADR-0058 - reading back a persisted synced-state blob.
 *
 * The folded state is persisted in two places: this device's own state file
 * and, inside the account backup, the copy a restore merges. Both hold the
 * bytes of `SyncedState.toBytes()`, and the core refuses any encoding
 * version but the one it writes (`STATE_VERSION` in
 * `crates/lakko-sync/src/synced/state.rs`, 7 since the conversation-end
 * register of ADR-0087 section 9; every older version is refused, not
 * migrated, because nothing is deployed).
 *
 * So a blob the core refuses is "no synced state held": the reader carries
 * on with nothing, the device catches up from its other devices or starts
 * empty, and the state file is replaced by the next write. It is never a
 * crash and never a reason to stop a sign-in or a restore.
 *
 * The two byte-level operations that need the native core sit behind
 * [SyncedStateBytes], so the rules below run in a host JVM test with no
 * native library.
 */

/** The native half: what `SyncedState` does with persisted bytes. */
internal interface SyncedStateBytes {
    /**
     * [bytes] decoded and re-encoded by this build's core
     * (`SyncedState.fromBytes(bytes).toBytes()`).
     * @throws Throwable when the core refuses [bytes] (another encoding
     * version, a damaged blob).
     */
    fun canonical(bytes: ByteArray): ByteArray

    /**
     * [backup] folded into [existing] as its own catch-up snapshot, encoded.
     * @throws Throwable when the core refuses either.
     */
    fun merged(existing: ByteArray, backup: ByteArray): ByteArray
}

/**
 * The synced state [stored] holds, as bytes this build's core reads, or
 * `null` when none is held: nothing stored, an empty blob, or one the core
 * refuses. A refusal is reported to [onRefused] once and is not thrown.
 */
internal fun heldSyncedStateBytes(
    stored: ByteArray?,
    core: SyncedStateBytes,
    onRefused: (Throwable) -> Unit,
): ByteArray? {
    if (stored == null || stored.isEmpty()) return null
    return try {
        core.canonical(stored)
    } catch (cause: Throwable) {
        onRefused(cause)
        null
    }
}

/**
 * The state a session starts from: what [stored] opens to, else a fresh one.
 * A blob the core refuses (a version this build does not read) is reported to
 * [onRefused] and is not a reason to stop a sign-in: the device starts clean,
 * catches up from its other devices, and the next write replaces the file.
 */
internal fun <T> seedStateOrFresh(
    stored: ByteArray?,
    open: (ByteArray) -> T,
    fresh: () -> T,
    onRefused: (Throwable) -> Unit,
): T {
    if (stored == null) return fresh()
    return try {
        open(stored)
    } catch (cause: Throwable) {
        onRefused(cause)
        fresh()
    }
}

/** Which blob a backup seed refused. */
internal enum class RefusedSyncedState {
    /** The copy inside the restored backup. */
    BACKUP,

    /** This device's own state file. */
    STORED,

    /** Both decoded, the fold of one into the other failed. */
    MERGE,
}

/**
 * What to write to the state file when a restored backup carried [backup]
 * and the file holds [stored] (ADR-0058 section 5.2), or `null` when there
 * is nothing to write.
 *
 * A refused [backup] seeds nothing and leaves the file alone. A refused
 * [stored] is "none held", so the backup's state replaces it. Each refusal
 * is reported to [onRefused] once and is not thrown.
 */
internal fun backupSeedBytes(
    stored: ByteArray?,
    backup: ByteArray,
    core: SyncedStateBytes,
    onRefused: (RefusedSyncedState, Throwable) -> Unit,
): ByteArray? {
    val restored = heldSyncedStateBytes(backup, core) { onRefused(RefusedSyncedState.BACKUP, it) }
        ?: return null
    val existing = heldSyncedStateBytes(stored, core) { onRefused(RefusedSyncedState.STORED, it) }
        ?: return restored
    return try {
        core.merged(existing, restored)
    } catch (cause: Throwable) {
        onRefused(RefusedSyncedState.MERGE, cause)
        null
    }
}
