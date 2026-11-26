// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.Json
import me.lakko.app.auth.MarkerStorage

/*
 * ADR-0101 section 9 - the history comes back after a key reset, newest
 * first. Mirror of `apps/web/src/chat/history-return.ts`.
 *
 * ## The requester (the reset device)
 *
 * Once it joined a conversation inside its own continuity window (every
 * conversation it joins then is a returned one: the reset identity knows
 * nobody else), it asks there for history: a field-less request
 * (`composeHistoryRequestEnvelope`) carrying a fresh ADR-0091 gap key and
 * `before` (0: the newest). The answer is read by the ADR-0091 gap look,
 * whose signer rule is the core's `gapHistorySignerAccepted`. Each page filed
 * moves `before` to the oldest DS id it carried and asks again; an empty page
 * ends it ([nextHistoryStep]).
 *
 * ## The answerer
 *
 * [historyAnswerAllowed]: only for a request from the CURRENT key of an
 * account whose reset this device accepted, whose retired key was in the
 * conversation's earlier group this device holds; in a one-to-one the other
 * person answers, in a group an owner or admin. The page is the earlier
 * group's rows below `before`, [HISTORY_PAGE_SIZE] at most, sealed to the
 * request's gap key for the successor ([historyPageRows]), posted to
 * `gapHistoryRendezvous(gap key)`. An EMPTY page is sealed and posted too.
 */

/** The frozen history-request domain separator, `lakko/history-request/v1`
 *  followed by one NUL byte. Built programmatically: a raw NUL in a source
 *  file makes git treat it as binary. */
internal val HISTORY_REQUEST_ENVELOPE_DOMAIN: ByteArray =
    "lakko/history-request/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

/** The fixed length of a history request (domain, version, gap key, before). */
internal val HISTORY_REQUEST_ENVELOPE_LEN: Int = HISTORY_REQUEST_ENVELOPE_DOMAIN.size + 1 + 32 + 8

/** Does this application plaintext open with the history-request domain and
 *  have its fixed length? A complete classification (the separator ends in
 *  NUL). Pure. */
internal fun looksLikeHistoryRequestEnvelope(bytes: ByteArray): Boolean {
    if (bytes.size != HISTORY_REQUEST_ENVELOPE_LEN) return false
    for (i in HISTORY_REQUEST_ENVELOPE_DOMAIN.indices) {
        if (bytes[i] != HISTORY_REQUEST_ENVELOPE_DOMAIN[i]) return false
    }
    return true
}

/** Rows per page (chosen, not measured). */
public const val HISTORY_PAGE_SIZE: Int = 200

/** Pages asked for at most per conversation (chosen: a stop for a peer that
 *  never sends an empty page). */
public const val HISTORY_MAX_PAGES: Int = 50

/** A request with no page after this long is posted again (chosen). */
public const val HISTORY_RESEND_MS: Long = 10L * 60 * 1000

/** A page asked for this many times with no answer is not asked for again
 *  (chosen). */
public const val HISTORY_MAX_ASKS: Int = 6

/**
 * Whether this device answers one history request.
 *
 * | from self | reset accepted | was in earlier group | earlier group held | kind  | own role       | answers |
 * |-----------|----------------|----------------------|--------------------|-------|----------------|---------|
 * | yes       | -              | -                    | -                  | -     | -              | no      |
 * | no        | no             | -                    | -                  | -     | -              | no      |
 * | no        | yes            | no                   | -                  | -     | -              | no      |
 * | no        | yes            | yes                  | no                 | -     | -              | no      |
 * | no        | yes            | yes                  | yes                | chat  | -              | yes     |
 * | no        | yes            | yes                  | yes                | group | owner or admin | yes     |
 * | no        | yes            | yes                  | yes                | group | member         | no      |
 */
public fun historyAnswerAllowed(
    fromSelf: Boolean,
    requesterResetAccepted: Boolean,
    requesterWasInEarlierGroup: Boolean,
    earlierGroupHeld: Boolean,
    kind: ConversationKind,
    ownIsOwnerOrAdmin: Boolean,
): Boolean {
    if (fromSelf || !requesterResetAccepted) return false
    if (!requesterWasInEarlierGroup || !earlierGroupHeld) return false
    return kind == ConversationKind.Chat || ownIsOwnerOrAdmin
}

/** The requester's state for one conversation. */
@Serializable
public data class HistoryRequestState(
    /** The next request's `before` (decimal string; "0" = the newest). */
    val before: String = "0",
    /** Pages asked for so far. */
    val pages: Int = 0,
    /** When the current request was posted (unix ms); 0 = not yet. */
    val sentAtMs: Long = 0L,
    /** How many times the current page was asked for with no answer. */
    val asks: Int = 0,
    /** No more pages. */
    val done: Boolean = false,
)

/** The first request of a returned conversation. */
public val FIRST_HISTORY_REQUEST: HistoryRequestState = HistoryRequestState()

/** The state after one more post of the current request. */
public fun historyAsked(state: HistoryRequestState, nowMs: Long): HistoryRequestState =
    state.copy(sentAtMs = nowMs, asks = state.asks + 1)

/** Whether the current request is to be posted now. */
public fun historyRequestDue(state: HistoryRequestState, nowMs: Long): Boolean {
    if (state.done || state.asks >= HISTORY_MAX_ASKS) return false
    return state.sentAtMs == 0L || nowMs < state.sentAtMs || nowMs - state.sentAtMs >= HISTORY_RESEND_MS
}

/**
 * What a filed page does to the state: the oldest DS id among its rows is
 * the next `before`; an empty page, or one whose oldest id is not below the
 * current `before`, or the page cap, ends it.
 */
public fun nextHistoryStep(state: HistoryRequestState, filedMessageIds: List<Long>): HistoryRequestState {
    if (state.done) return state
    val oldest = filedMessageIds.filter { it > 0L }.minOrNull()
    val before = state.before.toLongOrNull() ?: 0L
    val pages = state.pages + 1
    if (oldest == null || (before > 0L && oldest >= before) || pages >= HISTORY_MAX_PAGES) {
        return state.copy(pages = pages, done = true)
    }
    return HistoryRequestState(before = oldest.toString(), pages = pages, sentAtMs = 0L, asks = 0, done = false)
}

/**
 * The rows of one history page: DS id below [before] (0: no bound), the
 * newest [pageSize] of them, oldest first. Rows whose id is not positive are
 * left out. Pure.
 */
public fun <T> historyPageRows(rows: List<T>, idOf: (T) -> Long?, before: Long, pageSize: Int): List<T> {
    if (pageSize <= 0) return emptyList()
    return rows.mapNotNull { row -> idOf(row)?.takeIf { it > 0L && (before <= 0L || it < before) }?.let { row to it } }
        .sortedBy { it.second }
        .takeLast(pageSize)
        .map { it.first }
}

/** The requester's states, per signed-in account: group id hex -> state. */
public class HistoryReturnStore(private val storage: MarkerStorage) {
    private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }
    private val serializer = MapSerializer(String.serializer(), HistoryRequestState.serializer())

    /** Every conversation's state. */
    public fun all(account: String): Map<String, HistoryRequestState> {
        val raw = runCatching { storage.get(KEY_PREFIX + account) }.getOrNull() ?: return emptyMap()
        return runCatching { json.decodeFromString(serializer, raw) }.getOrElse { emptyMap() }
            .filterValues { it.before.matches(DECIMAL) }
    }

    /** One conversation's state, or `null`. */
    public fun get(account: String, groupIdHex: String): HistoryRequestState? = all(account)[groupIdHex]

    /** Write one conversation's state. Never throws. */
    public fun put(account: String, groupIdHex: String, state: HistoryRequestState) {
        val next = all(account) + (groupIdHex to state)
        try {
            me.lakko.app.account.AccountStoreGate.write(account) {
                storage.set(KEY_PREFIX + account, json.encodeToString(serializer, next))
            }
        } catch (_: Throwable) {
            // Asked again next poll; a duplicate page files once.
        }
    }

    /** Start asking for a returned conversation's history (idempotent). */
    public fun noteReturned(account: String, groupIdHex: String) {
        if (get(account, groupIdHex) == null) put(account, groupIdHex, FIRST_HISTORY_REQUEST)
    }

    /** Wipe. */
    public fun clear(account: String) {
        runCatching { storage.remove(KEY_PREFIX + account) }
    }

    public companion object {
        public const val KEY_PREFIX: String = "history-return:"
        private val DECIMAL = Regex("^[0-9]+$")

        public fun onDevice(context: Context): HistoryReturnStore =
            HistoryReturnStore(me.lakko.app.auth.ResetReturnStorage.plain(context))
    }
}
