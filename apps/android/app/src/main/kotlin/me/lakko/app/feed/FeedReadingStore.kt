// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.json.Json
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors

/**
 * [FeedReading] on disk, per account, sealed under the feed's Keystore key in
 * `noBackupFilesDir` like the feed's other state ([FeedSealedStore]): which
 * conversations are named here is what the person is in, so it is not a plain
 * preference.
 *
 * ```text
 *   noBackupFilesDir/lakko-feed/reading/<accountIdB64>.bin
 * ```
 *
 * One live [StateFlow] per account, read once and kept; every change is
 * applied in memory at once and written on a single background thread, the
 * newest state last. Best-effort: a failed write leaves the memory copy,
 * which the next change writes again.
 */
public object FeedReadingStore {
    internal const val DIR: String = "reading"

    private val json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    private val flows = ConcurrentHashMap<String, MutableStateFlow<FeedReading>>()
    private val writer = Executors.newSingleThreadExecutor { r ->
        Thread(r, "lakko-feed-reading").apply { isDaemon = true }
    }

    /** The account's reading state; loaded from disk on first use. */
    public fun flow(context: Context, accountIdB64: String): StateFlow<FeedReading> =
        flowOf(context.applicationContext, accountIdB64).asStateFlow()

    private fun flowOf(context: Context, accountIdB64: String): MutableStateFlow<FeedReading> =
        flows.getOrPut(accountIdB64) { MutableStateFlow(load(context, accountIdB64)) }

    private fun load(context: Context, accountIdB64: String): FeedReading {
        val text = FeedSealedStore.read(context, DIR, accountIdB64) ?: return FeedReading()
        val wire = try {
            json.decodeFromString(FeedReadingWire.serializer(), text)
        } catch (_: Throwable) {
            null
        }
        return decodeFeedReading(wire)
    }

    /** Apply [change] and persist the result when it differs. */
    public fun update(context: Context, accountIdB64: String, change: (FeedReading) -> FeedReading) {
        val app = context.applicationContext
        val flow = flowOf(app, accountIdB64)
        var changed = false
        while (true) {
            val before = flow.value
            val after = change(before)
            if (after == before) break
            if (flow.compareAndSet(before, after)) {
                changed = true
                break
            }
        }
        if (!changed) return
        writer.execute {
            // The newest state, whichever change queued this write.
            val latest = flows[accountIdB64]?.value ?: return@execute
            FeedSealedStore.write(
                app,
                DIR,
                accountIdB64,
                json.encodeToString(FeedReadingWire.serializer(), encodeFeedReading(latest)),
            )
        }
    }

    /** Forget device: the cached copy (the file goes with [FeedSealedStore.deleteAccount]). */
    internal fun forget(accountIdB64: String) {
        flows.remove(accountIdB64)
    }
}
