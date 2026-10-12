// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json

/*
 * ADR-0065 R.5 - a plain-preferences mirror of the lineage registers
 * (`successor group -> root conversation`) of the ADR-0058 synced state.
 *
 * The synced state itself is sealed under the device key and owned by the
 * DeviceGroup repository. Two readers cannot go through it: the chat
 * repository's own hot paths (every send asks which group is the
 * conversation's head) and the background push path, which opens no synced
 * state at all (C-59: it reads, and writes nothing). The mirror holds group
 * ids and stamps only: no key material, and nothing a group id does not
 * already reveal on this device. It is rewritten whenever the registers
 * change and removed with the account's data.
 */

@Serializable
private data class MirrorRow(val s: String, val r: String, val h: String)

private val mirrorJson = Json { ignoreUnknownKeys = true }

/** The registers as stored. Visible for the codec test. */
internal fun encodeLineageMirror(entries: List<LineageEntry>): String =
    mirrorJson.encodeToString(
        ListSerializer(MirrorRow.serializer()),
        entries.map { MirrorRow(it.successorHex, it.rootHex, it.hlc.toString()) },
    )

/** The inverse; a malformed row is dropped, an unreadable value is empty
 *  (every group is then its own conversation). */
internal fun decodeLineageMirror(raw: String?): List<LineageEntry> {
    if (raw.isNullOrEmpty()) return emptyList()
    return try {
        mirrorJson.decodeFromString(ListSerializer(MirrorRow.serializer()), raw).mapNotNull { row ->
            val hlc = row.h.toULongOrNull() ?: return@mapNotNull null
            if (row.s.isEmpty() || row.r.isEmpty()) null else LineageEntry(row.s.lowercase(), row.r.lowercase(), hlc)
        }
    } catch (_: Exception) {
        emptyList()
    }
}

public object LineageMirror {
    private const val PREFS = "lakko-lineage-mirror"
    private const val KEY_PREFIX = "lineage-v1:"

    /** The registers last saved or loaded per account, with the lineage
     *  built from them: every thread-list publish asks. */
    private val cache = java.util.concurrent.ConcurrentHashMap<String, Pair<List<LineageEntry>, Lineage>>()

    private val mutableVersion = kotlinx.coroutines.flow.MutableStateFlow(0L)

    /** Bumped whenever the mirror of any account changes. */
    public val version: kotlinx.coroutines.flow.StateFlow<Long> = mutableVersion

    /** The lineage of [accountIdB64] as this device holds it. */
    public fun lineage(context: Context, accountIdB64: String): Lineage =
        cache[accountIdB64]?.second ?: load(context, accountIdB64).let { cache[accountIdB64]?.second ?: buildLineage(it) }

    public fun save(context: Context, accountIdB64: String, entries: List<LineageEntry>) {
        cache[accountIdB64] = entries to buildLineage(entries)
        mutableVersion.value = mutableVersion.value + 1
        try {
            context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
                .edit().putString(KEY_PREFIX + accountIdB64, encodeLineageMirror(entries)).apply()
        } catch (_: Throwable) {
            // best-effort: the registers themselves are in the synced state
        }
    }

    public fun load(context: Context, accountIdB64: String): List<LineageEntry> {
        cache[accountIdB64]?.let { return it.first }
        val entries = try {
            decodeLineageMirror(
                context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
                    .getString(KEY_PREFIX + accountIdB64, null),
            )
        } catch (_: Throwable) {
            return emptyList()
        }
        cache[accountIdB64] = entries to buildLineage(entries)
        return entries
    }

    public fun deleteAccount(context: Context, accountIdB64: String) {
        cache.remove(accountIdB64)
        try {
            context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
                .edit().remove(KEY_PREFIX + accountIdB64).apply()
        } catch (_: Throwable) {
            // best-effort
        }
    }
}
