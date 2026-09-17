// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.Json

/**
 * ADR-0024 — durable per-account, per-group read watermarks.
 *
 * The author-side "Read" state needs every other member's highest asserted
 * `read_up_to`. The repository folds receipts into an in-memory map on the
 * group record, but the DS poll resumes past receipts it already applied, so
 * without this store a relaunch showed "Sent" on messages everyone had read
 * until each reader happened to open the chat again.
 *
 * Shape: `groupIdHex -> (senderSignatureKeyB64 -> readUpTo)`. Keyed on the
 * sender's signature key, never a leaf index (C-68): a leaf is reused by the
 * next member added, and a leaf-keyed watermark would hand them the departed
 * member's read history.
 *
 * MONOTONE: [merge] never lowers a stored value (the rule is
 * [foldReadWatermark]), so a stale or reordered write is harmless and two
 * concurrent writers converge whatever order they land in.
 *
 * Content is non-secret metadata the chat already renders; it never reaches a
 * server (invariants #1 / #2 unaffected). Plain `SharedPreferences`, file
 * `lakko-read-watermarks`, one key per group:
 * `v1:<accountIdB64>:<groupIdHex lowercase>` -> JSON object.
 *
 * Bounded naturally: one entry per sender per group, no pruning.
 */
public interface ReadWatermarkStore {
    /** The stored watermarks for one group; empty when none or undecodable. */
    public fun load(accountIdB64: String, groupIdHex: String): Map<MemberIdentity, Long>

    /** Every group's watermarks for [accountIdB64] (bootstrap seeding). */
    public fun loadAccount(accountIdB64: String): Map<String, Map<MemberIdentity, Long>>

    /** Monotone merge of [watermarks] into the stored map; returns the result. */
    public fun merge(
        accountIdB64: String,
        groupIdHex: String,
        watermarks: Map<MemberIdentity, Long>,
    ): Map<MemberIdentity, Long>

    /** Drop every group's watermarks for [accountIdB64] (forget device). */
    public fun deleteAccount(accountIdB64: String)
}

/** The production store over [SharedPreferences]. The preferences handle is
 *  opened lazily so constructing the repository on the main thread does not
 *  start a disk read. */
public class SharedPreferencesReadWatermarkStore(context: Context) : ReadWatermarkStore {

    private val appContext: Context = context.applicationContext

    private val prefs: SharedPreferences by lazy {
        appContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
    }

    override fun load(accountIdB64: String, groupIdHex: String): Map<MemberIdentity, Long> =
        decodeReadWatermarks(prefs.getString(key(accountIdB64, groupIdHex), null))

    override fun loadAccount(accountIdB64: String): Map<String, Map<MemberIdentity, Long>> {
        val prefix = accountPrefix(accountIdB64)
        val out = HashMap<String, Map<MemberIdentity, Long>>()
        for ((k, v) in prefs.all) {
            if (!k.startsWith(prefix)) continue
            val decoded = decodeReadWatermarks(v as? String)
            if (decoded.isNotEmpty()) out[k.removePrefix(prefix)] = decoded
        }
        return out
    }

    override fun merge(
        accountIdB64: String,
        groupIdHex: String,
        watermarks: Map<MemberIdentity, Long>,
    ): Map<MemberIdentity, Long> = synchronized(WRITE_LOCK) {
        val k = key(accountIdB64, groupIdHex)
        val stored = decodeReadWatermarks(prefs.getString(k, null))
        val merged = mergeReadWatermarks(stored, watermarks)
        if (merged != stored) {
            prefs.edit().putString(k, encodeReadWatermarks(merged)).apply()
        }
        merged
    }

    override fun deleteAccount(accountIdB64: String): Unit = synchronized(WRITE_LOCK) {
        val prefix = accountPrefix(accountIdB64)
        val editor = prefs.edit()
        for (k in prefs.all.keys) if (k.startsWith(prefix)) editor.remove(k)
        editor.apply()
    }

    private companion object {
        const val PREFS = "lakko-read-watermarks"

        /** Process-wide: the merge is a read-modify-write on one shared
         *  preferences file, whichever instance performs it. */
        val WRITE_LOCK = Any()

        fun accountPrefix(accountIdB64: String) = "v1:$accountIdB64:"
        fun key(accountIdB64: String, groupIdHex: String) =
            accountPrefix(accountIdB64) + groupIdHex.lowercase()
    }
}

/** Host-testable store with the same contract, sharing the codec. */
public class InMemoryReadWatermarkStore : ReadWatermarkStore {
    private val rows = HashMap<String, String>()

    private fun key(accountIdB64: String, groupIdHex: String) =
        "v1:$accountIdB64:${groupIdHex.lowercase()}"

    override fun load(accountIdB64: String, groupIdHex: String): Map<MemberIdentity, Long> =
        synchronized(this) { decodeReadWatermarks(rows[key(accountIdB64, groupIdHex)]) }

    override fun loadAccount(accountIdB64: String): Map<String, Map<MemberIdentity, Long>> =
        synchronized(this) {
            val prefix = "v1:$accountIdB64:"
            rows.filterKeys { it.startsWith(prefix) }
                .map { (k, v) -> k.removePrefix(prefix) to decodeReadWatermarks(v) }
                .filter { it.second.isNotEmpty() }
                .toMap()
        }

    override fun merge(
        accountIdB64: String,
        groupIdHex: String,
        watermarks: Map<MemberIdentity, Long>,
    ): Map<MemberIdentity, Long> = synchronized(this) {
        val k = key(accountIdB64, groupIdHex)
        val merged = mergeReadWatermarks(decodeReadWatermarks(rows[k]), watermarks)
        rows[k] = encodeReadWatermarks(merged)
        merged
    }

    override fun deleteAccount(accountIdB64: String): Unit = synchronized(this) {
        val prefix = "v1:$accountIdB64:"
        rows.keys.removeAll { it.startsWith(prefix) }
    }
}

// =============================================================================
// Pure rules — no Android dependency, unit-tested on the host JVM.
// =============================================================================

/** Monotone union: every entry of [incoming] folded into [stored] with
 *  [foldReadWatermark], so no stored value is ever lowered. */
internal fun mergeReadWatermarks(
    stored: Map<MemberIdentity, Long>,
    incoming: Map<MemberIdentity, Long>,
): Map<MemberIdentity, Long> {
    var acc = stored
    for ((who, upTo) in incoming) acc = foldReadWatermark(acc, who, upTo)
    return acc
}

private val readWatermarkJson = Json { ignoreUnknownKeys = true }

private val readWatermarkSerializer = MapSerializer(String.serializer(), Long.serializer())

/** The stored row: a JSON object `{ "<signatureKeyB64>": <readUpTo>, ... }`. */
internal fun encodeReadWatermarks(watermarks: Map<MemberIdentity, Long>): String =
    readWatermarkJson.encodeToString(readWatermarkSerializer, watermarks)

/** Inverse of [encodeReadWatermarks]; empty for an absent or malformed row. */
internal fun decodeReadWatermarks(raw: String?): Map<MemberIdentity, Long> {
    if (raw.isNullOrEmpty()) return emptyMap()
    return try {
        readWatermarkJson.decodeFromString(readWatermarkSerializer, raw)
            .filterKeys { it.isNotEmpty() }
    } catch (_: Exception) {
        emptyMap()
    }
}
