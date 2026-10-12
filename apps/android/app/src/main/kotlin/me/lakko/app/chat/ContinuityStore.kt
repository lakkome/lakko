// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/*
 * ADR-0065 R.5 / R.7 - the facts about a moved conversation that are this
 * device's alone and that nothing else records:
 *
 *   * WHEN this device learned an old group was continued elsewhere: the
 *     start of the window in which an old GROUP's journal is still read
 *     ([oldSegmentPolled]);
 *   * that the founder's re-send of its own messages into the successor of a
 *     one-to-one RAN ([messagesToResend]), so a second pass never sends them
 *     twice;
 *   * the first half of a member's device swap that landed in a commit of its
 *     own, so the second half leaves no `Members changed` row either
 *     ([deviceSwapAcrossCommits]);
 *   * that an old group's push subscription was retired here.
 *
 * Per account, group ids and timestamps only. Mirror of
 * `apps/web/src/chat/continuity-store.ts`.
 */

/** One account's continuity facts. Group ids are lower-case hex. */
@Serializable
public data class ContinuityRow(
    /** Old group -> when this device learned it moved (ms). */
    val moved: Map<String, Long> = emptyMap(),
    /** Old groups whose re-send into the successor ran. */
    val resent: Set<String> = emptySet(),
    /** Group -> the people whose superseded devices left it and whose new
     *  devices have not arrived: account -> since when (ms). */
    val swapping: Map<String, Map<String, Long>> = emptyMap(),
    /** Old groups whose push subscription was retired here. */
    val pushRetired: Set<String> = emptySet(),
    /** The moved groups that are ONE-TO-ONES: their old journal is not read
     *  again and a wake for one shows nothing ([oldSegmentPolled]). Kept as a
     *  plain fact so the background push path, which opens no MLS state for
     *  this question, can answer it. */
    val movedOneToOne: Set<String> = emptySet(),
)

/** Record that [oldGroupHex] moved; the FIRST time wins. */
public fun ContinuityRow.withMoved(oldGroupHex: String, nowMs: Long, oneToOne: Boolean = false): ContinuityRow {
    val stamped = if (moved.containsKey(oldGroupHex)) this else copy(moved = moved + (oldGroupHex to nowMs))
    return if (!oneToOne || oldGroupHex in stamped.movedOneToOne) {
        stamped
    } else {
        stamped.copy(movedOneToOne = stamped.movedOneToOne + oldGroupHex)
    }
}

public fun ContinuityRow.withResent(oldGroupHex: String): ContinuityRow =
    if (oldGroupHex in resent) this else copy(resent = resent + oldGroupHex)

public fun ContinuityRow.withPushRetired(oldGroupHex: String): ContinuityRow =
    if (oldGroupHex in pushRetired) this else copy(pushRetired = pushRetired + oldGroupHex)

/** Replace a group's awaited swaps (an empty memory removes its entry). */
public fun ContinuityRow.withAwaitedSwaps(groupHex: String, awaited: Map<String, Long>): ContinuityRow =
    when {
        awaited.isEmpty() -> if (swapping.containsKey(groupHex)) copy(swapping = swapping - groupHex) else this
        swapping[groupHex] == awaited -> this
        else -> copy(swapping = swapping + (groupHex to awaited))
    }

private val continuityJson = Json { ignoreUnknownKeys = true }

internal fun encodeContinuityRow(row: ContinuityRow): String =
    continuityJson.encodeToString(ContinuityRow.serializer(), row)

/** An unreadable row reads as empty: the facts are then per session. */
internal fun decodeContinuityRow(raw: String?): ContinuityRow {
    if (raw.isNullOrEmpty()) return ContinuityRow()
    return try {
        continuityJson.decodeFromString(ContinuityRow.serializer(), raw)
    } catch (_: Exception) {
        ContinuityRow()
    }
}

/** Where a continuity row lives. */
public interface ContinuityBackend {
    public fun load(accountIdB64: String): ContinuityRow

    public fun save(accountIdB64: String, row: ContinuityRow)
}

public class InMemoryContinuityBackend : ContinuityBackend {
    private val rows = HashMap<String, ContinuityRow>()

    override fun load(accountIdB64: String): ContinuityRow = synchronized(rows) { rows[accountIdB64] ?: ContinuityRow() }

    override fun save(accountIdB64: String, row: ContinuityRow) {
        synchronized(rows) { rows[accountIdB64] = row }
    }
}

public class SharedPrefsContinuityBackend(context: Context) : ContinuityBackend {
    private val appContext = context.applicationContext

    private fun prefs(): SharedPreferences = appContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    override fun load(accountIdB64: String): ContinuityRow =
        try {
            decodeContinuityRow(prefs().getString(KEY_PREFIX + accountIdB64, null))
        } catch (_: Throwable) {
            ContinuityRow()
        }

    override fun save(accountIdB64: String, row: ContinuityRow) {
        try {
            prefs().edit().putString(KEY_PREFIX + accountIdB64, encodeContinuityRow(row)).apply()
        } catch (_: Throwable) {
            // storage unavailable: the facts are per session then
        }
    }

    public companion object {
        public const val PREFS: String = "lakko-reset-continuity"
        private const val KEY_PREFIX = "continuity-v1:"

        /** Forget everything for the account (its data is wiped). */
        public fun deleteAccount(context: Context, accountIdB64: String) {
            try {
                context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
                    .edit().remove(KEY_PREFIX + accountIdB64).apply()
            } catch (_: Throwable) {
                // best-effort
            }
        }
    }
}

/** The account's continuity facts, read-modify-written under one lock. */
public class ContinuityStore(private val backend: ContinuityBackend) {
    private val lock = Any()

    public fun row(accountIdB64: String): ContinuityRow = synchronized(lock) { backend.load(accountIdB64) }

    public fun update(accountIdB64: String, change: (ContinuityRow) -> ContinuityRow) {
        synchronized(lock) {
            val held = backend.load(accountIdB64)
            val next = change(held)
            if (next != held) backend.save(accountIdB64, next)
        }
    }
}
