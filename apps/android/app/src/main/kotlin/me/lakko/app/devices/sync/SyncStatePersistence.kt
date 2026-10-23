// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.launch

/**
 * The debounced writer of the folded synced state (ADR-0058: folders, flags,
 * read cursors, drafts, order) for ONE bound account.
 *
 * Every change bumps [versions]; after [debounceMs] of quiet the state is
 * encoded and handed to [write]. Log out keeps this state for the account's
 * next sign-in (the restored DeviceGroup snapshot has already consumed the
 * journal rows that built it, so the journal cannot rebuild it), so
 * [stopAndFlush] writes whatever change is still waiting out its debounce
 * before it lets the account go. After that nothing is written for the
 * account again: every write checks, under [lock], that the account is still
 * the bound one, and the flush is the one that unbinds it. Forget device and
 * Delete account stop this writer first and then delete the file
 * ([DeviceGroupSyncRepository.deleteAccountRows]), so a deleted state stays
 * deleted.
 *
 * [onChanged] runs after each write of a change made since [start] (not after
 * the first write, which only writes back what bootstrap read): the backup
 * carries these bytes, so the caller marks it dirty.
 */
internal class SyncStatePersistence(
    private val scope: CoroutineScope,
    private val versions: StateFlow<Long>,
    private val debounceMs: Long,
    private val encode: () -> ByteArray?,
    private val write: (accountIdB64: String, bytes: ByteArray) -> Unit,
    private val onChanged: (accountIdB64: String) -> Unit,
) {
    private val lock = Any()

    /** The account written for, or `null` when stopped. Guarded by [lock]. */
    private var boundAccount: String? = null

    /** The version last written for [boundAccount]. Guarded by [lock]. */
    private var writtenVersion: Long = NOT_WRITTEN

    /** The version when [start] bound the account. Guarded by [lock]. */
    private var startVersion: Long = NOT_WRITTEN

    @Volatile private var job: Job? = null

    /** Bind [accountIdB64] and start writing its changes. A different
     *  account still bound is flushed and let go first. */
    @OptIn(FlowPreview::class)
    fun start(accountIdB64: String) {
        stopAndFlush()
        synchronized(lock) {
            boundAccount = accountIdB64
            writtenVersion = NOT_WRITTEN
            startVersion = versions.value
        }
        job = scope.launch {
            versions.debounce(debounceMs).collect {
                synchronized(lock) {
                    if (boundAccount == accountIdB64) writeLocked(accountIdB64)
                }
            }
        }
    }

    /**
     * Stop the writer, write a change still waiting out its debounce, and let
     * the account go. Idempotent; a no-op when nothing is bound or nothing
     * changed since the last write. Blocks for one write at most.
     */
    fun stopAndFlush() {
        job?.cancel()
        job = null
        synchronized(lock) {
            val account = boundAccount ?: return
            boundAccount = null
            if (versions.value != writtenVersion) writeLocked(account)
        }
    }

    /** Whether an account is bound (tests). */
    val isBound: Boolean
        get() = synchronized(lock) { boundAccount != null }

    private fun writeLocked(accountIdB64: String): Boolean {
        // The version is read before the encode: the fold bumps it after the
        // state it counts has changed, so the bytes hold at least this much.
        val version = versions.value
        val bytes = encode() ?: return false
        val written = runCatching { write(accountIdB64, bytes) }
            .onFailure { Log.w(LOG_TAG, "device-sync snapshot persist failed (${it.javaClass.simpleName})") }
            .isSuccess
        if (written) writtenVersion = version
        if (version != startVersion) {
            runCatching { onChanged(accountIdB64) }
        }
        return written
    }

    /**
     * Write the snapshot now, past the debounce. `false` when nothing is
     * bound or the write failed. For a change that must be on disk before
     * anything depends on it: an invite link's record, before the link is
     * handed out (this device never reads its own record back from the
     * journal, so a process ended within the debounce forgot the link and
     * every request through it waited for ever).
     */
    fun writeNow(): Boolean = synchronized(lock) {
        val account = boundAccount ?: return false
        writeLocked(account)
    }

    private companion object {
        const val LOG_TAG = "DeviceGroupSync"
        const val NOT_WRITTEN: Long = Long.MIN_VALUE
    }
}
