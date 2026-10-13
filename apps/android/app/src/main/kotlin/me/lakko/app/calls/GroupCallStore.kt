// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import android.content.SharedPreferences
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * Durable per-account, per-group group-call records ([GroupCallRecord]).
 *
 * The ledger is folded from journal rows, and the DS poll resumes from its
 * cursor, so without this store a relaunch forgot every ongoing call: the chat
 * showed no Join banner and its call button started a second call. The
 * repository writes a group's record whenever it changes and seeds the ledger
 * from here at bootstrap; records past [GROUP_CALL_MAX_AGE_MS] are dropped on
 * load.
 *
 * Content is non-secret metadata the chat already renders (call id, media
 * flags, start time, member identities with their last Join time); it never
 * reaches a server (invariants #1 / #2 unaffected). Plain `SharedPreferences`,
 * file `lakko-group-calls`, one key per group:
 * `v1:<accountIdB64>:<groupIdHex lowercase>` -> JSON object. A group with no
 * record has no key.
 */
public interface GroupCallStore {
    /** Every group's record for [accountIdB64], undecodable rows skipped. */
    public fun loadAccount(accountIdB64: String): Map<String, GroupCallRecordRow>

    /** Replace [groupIdHex]'s record; `null` removes it. */
    public fun save(accountIdB64: String, groupIdHex: String, record: GroupCallRecordRow?)

    /** Drop every group's record for [accountIdB64] (forget device). */
    public fun deleteAccount(accountIdB64: String)
}

/** The stored shape of one [GroupCallRecord]. Public only because the store
 *  interface is; build one with [toRow] and read it with [toRecord]. */
@Serializable
public data class GroupCallRecordRow(
    val callIdHex: String,
    val audio: Boolean,
    val video: Boolean,
    val initiatorLeaf: Long,
    val startedAtMs: Long,
    val joinedMembers: Map<String, GroupCallMemberRow>,
    val lastActivityMs: Long,
    /** See [GroupCallRecord.initiatorPersonKey]. Absent from a row stored
     *  before the starter was recorded, which then reads as unknown. */
    val initiatorPersonKey: String? = null,
)

/** The stored shape of one [GroupCallMember]. */
@Serializable
public data class GroupCallMemberRow(val personKey: String, val lastSeenMs: Long)

internal fun GroupCallRecord.toRow(): GroupCallRecordRow = GroupCallRecordRow(
    callIdHex = callIdHex,
    audio = audio,
    video = video,
    initiatorLeaf = initiatorLeaf.toLong(),
    startedAtMs = startedAtMs,
    joinedMembers = joinedMembers.mapValues { (_, m) -> GroupCallMemberRow(m.personKey, m.lastSeenMs) },
    lastActivityMs = lastActivityMs,
    initiatorPersonKey = initiatorPersonKey,
)

/** `null` for a row that cannot be a record (out-of-range leaf, bad call id). */
internal fun GroupCallRecordRow.toRecord(): GroupCallRecord? {
    if (initiatorLeaf < 0 || initiatorLeaf > UInt.MAX_VALUE.toLong()) return null
    if (callIdBytesOf(callIdHex) == null || callIdHex.isEmpty()) return null
    return GroupCallRecord(
        callIdHex = callIdHex.lowercase(),
        audio = audio,
        video = video,
        initiatorLeaf = initiatorLeaf.toUInt(),
        startedAtMs = startedAtMs,
        joinedMembers = joinedMembers
            .filter { (key, m) -> key.isNotEmpty() && m.personKey.isNotEmpty() }
            .mapValues { (_, m) -> GroupCallMember(m.personKey, m.lastSeenMs) },
        lastActivityMs = lastActivityMs,
        initiatorPersonKey = initiatorPersonKey?.takeIf { it.isNotEmpty() },
    )
}

/**
 * The records a bootstrap seeds the ledger with: decodable rows younger than
 * [GROUP_CALL_MAX_AGE_MS] at [nowMs], keyed by lowercase group id hex.
 */
internal fun groupCallRecordsToSeed(
    rows: Map<String, GroupCallRecordRow>,
    nowMs: Long,
): Map<String, GroupCallRecord> {
    val out = HashMap<String, GroupCallRecord>()
    for ((groupIdHex, row) in rows) {
        val record = row.toRecord() ?: continue
        if (nowMs - record.startedAtMs >= GROUP_CALL_MAX_AGE_MS) continue
        out[groupIdHex.lowercase()] = record
    }
    return out
}

private val groupCallJson = Json { ignoreUnknownKeys = true }

internal fun encodeGroupCallRow(row: GroupCallRecordRow): String =
    groupCallJson.encodeToString(GroupCallRecordRow.serializer(), row)

/** Inverse of [encodeGroupCallRow]; `null` for an absent or malformed row. */
internal fun decodeGroupCallRow(raw: String?): GroupCallRecordRow? {
    if (raw.isNullOrEmpty()) return null
    return try {
        groupCallJson.decodeFromString(GroupCallRecordRow.serializer(), raw)
    } catch (_: Exception) {
        null
    }
}

private fun groupCallAccountPrefix(accountIdB64: String) = "v1:$accountIdB64:"

private fun groupCallKey(accountIdB64: String, groupIdHex: String) =
    groupCallAccountPrefix(accountIdB64) + groupIdHex.lowercase()

/** The production store over [SharedPreferences], opened lazily so building the
 *  repository on the main thread starts no disk read. */
public class SharedPreferencesGroupCallStore(context: Context) : GroupCallStore {

    private val appContext: Context = context.applicationContext

    private val prefs: SharedPreferences by lazy {
        appContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
    }

    override fun loadAccount(accountIdB64: String): Map<String, GroupCallRecordRow> {
        val prefix = groupCallAccountPrefix(accountIdB64)
        val out = HashMap<String, GroupCallRecordRow>()
        for ((k, v) in prefs.all) {
            if (!k.startsWith(prefix)) continue
            decodeGroupCallRow(v as? String)?.let { out[k.removePrefix(prefix)] = it }
        }
        return out
    }

    override fun save(accountIdB64: String, groupIdHex: String, record: GroupCallRecordRow?) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val key = groupCallKey(accountIdB64, groupIdHex)
            val editor = prefs.edit()
            if (record == null) editor.remove(key) else editor.putString(key, encodeGroupCallRow(record))
            editor.apply()
    
        }
    }

    override fun deleteAccount(accountIdB64: String) {
        val prefix = groupCallAccountPrefix(accountIdB64)
        val editor = prefs.edit()
        for (k in prefs.all.keys) if (k.startsWith(prefix)) editor.remove(k)
        editor.apply()
    }

    private companion object {
        const val PREFS = "lakko-group-calls"
    }
}

/** Host-testable store with the same contract, sharing the codec. */
public class InMemoryGroupCallStore : GroupCallStore {
    private val rows = HashMap<String, String>()

    override fun loadAccount(accountIdB64: String): Map<String, GroupCallRecordRow> = synchronized(this) {
        val prefix = groupCallAccountPrefix(accountIdB64)
        val out = HashMap<String, GroupCallRecordRow>()
        for ((k, v) in rows) {
            if (!k.startsWith(prefix)) continue
            decodeGroupCallRow(v)?.let { out[k.removePrefix(prefix)] = it }
        }
        out
    }

    override fun save(accountIdB64: String, groupIdHex: String, record: GroupCallRecordRow?): Unit =
        synchronized(this) {
            val key = groupCallKey(accountIdB64, groupIdHex)
            if (record == null) rows.remove(key) else rows[key] = encodeGroupCallRow(record)
        }

    override fun deleteAccount(accountIdB64: String): Unit = synchronized(this) {
        val prefix = groupCallAccountPrefix(accountIdB64)
        rows.keys.removeAll { it.startsWith(prefix) }
    }
}
