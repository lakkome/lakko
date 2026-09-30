// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discover

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import me.lakko.app.feed.FeedSealedStore

/**
 * Discover's two per-account rows on disk (ADR-0059, docs/ux/feed.md section
 * 6), sealed under the feed's Keystore-resident AES-256-GCM key in
 * `noBackupFilesDir`, like the feed's own state:
 *
 * ```text
 *   noBackupFilesDir/lakko-feed/
 *     discover-prefs/<accountIdB64>.bin    DiscoverPrefs (picked topics, decoys, cache)
 *     discover-shared/<accountIdB64>.bin   SharedChannelsRow (switches, lists)
 * ```
 *
 * Mirror of the web `lakko-discover` IndexedDB rows. Both are deleted by
 * forget-device. Best-effort: a read failure reads as empty.
 */
public object DiscoverStore {
    private const val PREFS_DIR = "discover-prefs"
    private const val SHARED_DIR = "discover-shared"

    private val sharedVersion = MutableStateFlow(0L)

    /** Bumped on every write of the shared-channels row. */
    public val sharedChanged: StateFlow<Long> = sharedVersion.asStateFlow()

    public fun loadPrefs(context: Context, accountIdB64: String): DiscoverPrefs =
        decodeDiscoverPrefs(FeedSealedStore.read(context, PREFS_DIR, accountIdB64))

    public fun savePrefs(context: Context, accountIdB64: String, prefs: DiscoverPrefs): Boolean =
        FeedSealedStore.write(context, PREFS_DIR, accountIdB64, encodeDiscoverPrefs(prefs))

    public fun loadShared(context: Context, accountIdB64: String): SharedChannelsRow =
        FeedSealedStore.read(context, SHARED_DIR, accountIdB64)?.let(::decodeSharedChannelsRow)
            ?: SharedChannelsRow()

    public fun saveShared(context: Context, accountIdB64: String, row: SharedChannelsRow): Boolean {
        val ok = FeedSealedStore.write(context, SHARED_DIR, accountIdB64, encodeSharedChannelsRow(row))
        if (ok) sharedVersion.update { it + 1 }
        return ok
    }

    /** Forget device: both rows of the account. */
    public fun deleteAccount(context: Context, accountIdB64: String) {
        FeedSealedStore.delete(context, PREFS_DIR, accountIdB64)
        FeedSealedStore.delete(context, SHARED_DIR, accountIdB64)
        sharedVersion.update { it + 1 }
    }
}
