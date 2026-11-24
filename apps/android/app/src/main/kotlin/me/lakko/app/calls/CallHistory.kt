// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.chat.ConversationKind
import java.time.Instant
import java.time.ZoneId

/**
 * Recent calls, across every conversation, newest first.
 *
 * `docs/ux/navigation.md` sections 5 and 6.2. A call belongs to a conversation,
 * so the tempting place to keep it is inside that conversation's transcript —
 * which is exactly where [CallTranscriptEntry] already lives, appended to the
 * encrypted message log as a system row. Reading the Calls screen off that
 * would mean walking every transcript on every open, and getting slower with
 * every call ever made.
 *
 * So the spec asks for two things of this list, and both are here:
 *
 *  * **an index, not a scan** — one record appended when the transcript row is
 *    written ([me.lakko.app.chat.ChatRepository.recordCallRow]), read back
 *    already in order;
 *  * **the parts, not the sentence** — a list sorts and filters on an outcome
 *    and a timestamp, and a row that stored the finished words ("Missed call")
 *    could be sorted on neither. The words are derived by [callRowText], the
 *    same function the conversation timeline uses, so the two never disagree
 *    about what a call was.
 *
 * Pure and Android-free, so the codec, the cap, the day grouping and the labels
 * are all unit-tested on the host JVM. The store that persists it is
 * [CallHistoryStore].
 */

/**
 * One call, as the cross-conversation index records it.
 *
 * Two kinds of row share the type. A call placed inside a conversation names
 * that conversation ([conversationIdHex], [kind]). A call with no conversation
 * (`docs/ux/navigation.md` section 5.1.1: started with `New call`, or joined
 * from a call link) names none: [conversationIdHex] and [kind] are `null`, and
 * it carries the call's own id ([callIdHex]) instead, so the same call is
 * filed once. A call has no name of its own, so such a row is always titled
 * `Group call`. Build one with [noConversationCallEntry].
 */
public data class CallHistoryEntry(
    /**
     * The conversation the call belongs to; the group id in lowercase hex.
     * `null` for a call with no conversation.
     */
    val conversationIdHex: String?,
    /** Which screen the row opens — the conversation's persisted kind.
     *  `null` for a call with no conversation. */
    val kind: ConversationKind?,
    val outcome: CallOutcome,
    /** The call carried video. Picks the call-back mode and the row's icon. */
    val video: Boolean,
    /** When the call happened, in unix millis. The sort key. */
    val startedAtMs: Long,
    /** How long media flowed, or `null` when the call never connected. */
    val durationSecs: Long? = null,
    /**
     * A call with no conversation: the call's 16-byte `call_id` in lowercase
     * hex, which is what makes a second write for the same call a no-op
     * ([callHistoryWith]). `null` on a conversation row.
     */
    val callIdHex: String? = null,
)

/** The row belongs to no conversation (section 5.1.1). */
public val CallHistoryEntry.hasNoConversation: Boolean
    get() = conversationIdHex == null

/**
 * A row for a call with no conversation (`docs/ux/navigation.md` section 5.1.1).
 *
 * [outgoing] is `true` when this device started the call (`New call`) and
 * `false` when it joined from a link. Such a call never rings, so it is never
 * missed or declined: the outcome is one of the two directions only.
 */
public fun noConversationCallEntry(
    callIdHex: String,
    outgoing: Boolean,
    video: Boolean,
    startedAtMs: Long,
    durationSecs: Long?,
): CallHistoryEntry = CallHistoryEntry(
    conversationIdHex = null,
    kind = null,
    outcome = if (outgoing) CallOutcome.OUTGOING else CallOutcome.INCOMING,
    video = video,
    startedAtMs = startedAtMs,
    durationSecs = durationSecs,
    callIdHex = callIdHex.lowercase(),
)

/** The transcript row this index entry stands for. */
public fun CallHistoryEntry.toTranscriptEntry(): CallTranscriptEntry =
    CallTranscriptEntry(outcome = outcome, video = video, durationSecs = durationSecs)

/**
 * How many calls the index keeps. Older calls stay in their conversation, which
 * is where they were written in the first place.
 */
public const val CALL_HISTORY_MAX: Int = 300

/** The `|`-separated record version of a conversation row. */
private const val RECORD_VERSION = "v1"

/** The record tag of a row for a call with no conversation (section 5.1.1). */
private const val NO_CONVERSATION_RECORD = "n1"
private const val SEPARATOR = '|'

/**
 * One stored line.
 *
 * Positional and `|`-separated: a hex id, two enum names, a flag and two
 * numbers, none of which can contain a `|`, so the split is unambiguous
 * without escaping.
 *
 * A call with no conversation is its own record, tagged [NO_CONVERSATION_RECORD]:
 * `n1|<call id hex>|<outcome>|<video>|<started>|<duration>`.
 */
public fun encodeCallHistoryEntry(entry: CallHistoryEntry): String {
    val conversation = entry.conversationIdHex
    if (conversation == null) {
        return listOf(
            NO_CONVERSATION_RECORD,
            (entry.callIdHex ?: "").lowercase(),
            entry.outcome.name,
            if (entry.video) "1" else "0",
            entry.startedAtMs.toString(),
            entry.durationSecs?.toString() ?: "",
        ).joinToString(SEPARATOR.toString())
    }
    return listOf(
        RECORD_VERSION,
        conversation.lowercase(),
        (entry.kind ?: ConversationKind.Group).name,
        entry.outcome.name,
        if (entry.video) "1" else "0",
        entry.startedAtMs.toString(),
        entry.durationSecs?.toString() ?: "",
    ).joinToString(SEPARATOR.toString())
}

private fun decodeFlag(text: String): Boolean? = when (text) {
    "1" -> true
    "0" -> false
    else -> null
}

private fun decodeDuration(text: String): Result<Long?> = if (text.isEmpty()) {
    Result.success(null)
} else {
    text.toLongOrNull()?.takeIf { it >= 0L }?.let { Result.success(it) }
        ?: Result.failure(IllegalArgumentException("duration"))
}

/**
 * Read one stored line, or `null` when it is not one this build understands.
 *
 * Fail-closed and silent: a record written by a newer build, or one a partial
 * write truncated, drops out of the list rather than throwing. The call it
 * stood for is still in its conversation's transcript, so nothing is lost that
 * cannot be found; crashing the Calls screen over one bad line would lose all
 * the others.
 */
public fun decodeCallHistoryEntry(line: String): CallHistoryEntry? {
    val parts = line.split(SEPARATOR)
    if (parts.firstOrNull() == NO_CONVERSATION_RECORD) return decodeNoConversationEntry(parts)
    if (parts.size != 7) return null
    if (parts[0] != RECORD_VERSION) return null
    val id = parts[1].trim().lowercase()
    if (id.isEmpty()) return null
    val kind = ConversationKind.entries.firstOrNull { it.name == parts[2] } ?: return null
    val outcome = CallOutcome.entries.firstOrNull { it.name == parts[3] } ?: return null
    val video = decodeFlag(parts[4]) ?: return null
    val startedAtMs = parts[5].toLongOrNull() ?: return null
    val duration = decodeDuration(parts[6]).getOrElse { return null }
    return CallHistoryEntry(
        conversationIdHex = id,
        kind = kind,
        outcome = outcome,
        video = video,
        startedAtMs = startedAtMs,
        durationSecs = duration,
    )
}

/** The `n1` record of a call with no conversation: six fields. */
private fun decodeNoConversationEntry(parts: List<String>): CallHistoryEntry? {
    if (parts.size != 6) return null
    val callId = parts[1].trim().lowercase()
    if (callId.isEmpty() || !callId.all { it in '0'..'9' || it in 'a'..'f' }) return null
    // Such a call never rings: only the two directions are a record.
    val outcome = when (parts[2]) {
        CallOutcome.OUTGOING.name -> CallOutcome.OUTGOING
        CallOutcome.INCOMING.name -> CallOutcome.INCOMING
        else -> return null
    }
    val video = decodeFlag(parts[3]) ?: return null
    val startedAtMs = parts[4].toLongOrNull() ?: return null
    val duration = decodeDuration(parts[5]).getOrElse { return null }
    return noConversationCallEntry(
        callIdHex = callId,
        outgoing = outcome == CallOutcome.OUTGOING,
        video = video,
        startedAtMs = startedAtMs,
        durationSecs = duration,
    )
}

/**
 * The list after [entry] is added: newest first, capped at [max].
 *
 * Ties break on the conversation id (or, for a call with no conversation, the
 * call id) so the order is total. Two calls that started in the same
 * millisecond — a pair placed by a script, two rows restored from a blob that
 * rounded — would otherwise sit in whatever order the store happened to yield,
 * and a list that reshuffles on a refresh reads as a bug and is impossible to
 * report.
 *
 * A call with no conversation is filed once per call (section 5.1.1): when a
 * row for the same [CallHistoryEntry.callIdHex] is already held, [entry] is
 * dropped and the first write stands.
 */
public fun callHistoryWith(
    existing: List<CallHistoryEntry>,
    entry: CallHistoryEntry,
    max: Int = CALL_HISTORY_MAX,
): List<CallHistoryEntry> {
    val callId = entry.callIdHex
    if (callId != null && existing.any { it.callIdHex.equals(callId, ignoreCase = true) }) {
        return callHistoryOrdered(existing, max)
    }
    return callHistoryOrdered(existing + entry, max)
}

/**
 * The list after a call that came from another device of this account (a
 * history bundle, placeholder version 7) is added. A transcript row carries
 * no call id, so the same call is recognised by its conversation, outcome,
 * mode and time: a bundle that lands twice, or a bundle row the one-time
 * fill already filed, files nothing more.
 */
public fun callHistoryWithRestored(
    existing: List<CallHistoryEntry>,
    entry: CallHistoryEntry,
    max: Int = CALL_HISTORY_MAX,
): List<CallHistoryEntry> {
    val held = existing.any {
        it.callIdHex == null &&
            it.conversationIdHex.equals(entry.conversationIdHex, ignoreCase = true) &&
            it.outcome == entry.outcome &&
            it.video == entry.video &&
            it.startedAtMs == entry.startedAtMs
    }
    if (held) return callHistoryOrdered(existing, max)
    return callHistoryWith(existing, entry, max)
}

/** The key a call row is filed under: its conversation and its message id
 *  (an old row's small local id is unique only inside its conversation). */
public fun callRowFiledKey(conversationIdHex: String, messageId: Long): String =
    "${conversationIdHex.lowercase()}:$messageId"

/** How many filed call row ids are kept; the oldest go first (chosen, not
 *  measured: far above [CALL_HISTORY_MAX], so a row still in the list is
 *  never forgotten). */
public const val CALL_ROWS_FILED_MAX: Int = 10_000

/** The filed call row ids as stored: comma-separated, oldest first, the
 *  newest [CALL_ROWS_FILED_MAX] kept. */
public fun encodeCallRowsFiled(filed: Collection<String>): String =
    filed.toList().takeLast(CALL_ROWS_FILED_MAX).joinToString(",")

/** [encodeCallRowsFiled] read back; an empty item is skipped. */
public fun decodeCallRowsFiled(raw: String?): LinkedHashSet<String> =
    raw.orEmpty().split(',').mapNotNullTo(LinkedHashSet()) { k -> k.trim().takeIf { it.isNotEmpty() } }

/**
 * What [CallHistoryStore] holds for one account: the list, and the call row
 * keys ([callRowFiledKey]) already offered to it. The two must agree: a key
 * that stays filed after its row left the list keeps that row out for good,
 * because every later filing of it ([callIndexFiled]) is skipped as a repeat.
 */
public data class CallIndexState(
    val entries: List<CallHistoryEntry>,
    val filed: LinkedHashSet<String>,
)

/** The filed keys of [conversationIdHex] (`"<hex>:<message id>"`). */
private fun isFiledKeyOf(key: String, conversationIdHex: String): Boolean =
    key.startsWith(conversationIdHex.lowercase() + ":", ignoreCase = true)

/**
 * The index after one conversation's calls leave it (ADR-0087 section 9: the
 * group was wiped on this device). Its filed keys go with them: a row of that
 * conversation that comes back (a rejoin's history, a bundle) is a new arrival
 * and is filed again. Found on the emulator 2026-11-24: a membership end and
 * rejoin emptied the list while the key stayed, so the call row back in the
 * conversation never reached Calls.
 */
public fun callIndexWithoutConversation(state: CallIndexState, conversationIdHex: String): CallIndexState =
    CallIndexState(
        entries = callHistoryWithout(state.entries, conversationIdHex),
        filed = state.filed.filterNotTo(LinkedHashSet()) { isFiledKeyOf(it, conversationIdHex) },
    )

/**
 * The index after transcript call [rows] (`filed key -> entry`) are filed: a
 * row whose key is already filed files nothing (a bundle written twice, a call
 * this device recorded live, which [ChatRepository][me.lakko.app.chat.ChatRepository]
 * filed under its start time and marked).
 */
public fun callIndexFiled(state: CallIndexState, rows: List<Pair<String, CallHistoryEntry>>): CallIndexState {
    val filed = LinkedHashSet(state.filed)
    var next = state.entries
    for ((key, entry) in rows) {
        if (!filed.add(key)) continue
        next = callHistoryWithRestored(next, entry)
    }
    return CallIndexState(next, filed)
}

/**
 * The one-time repair of an index an older build damaged: a conversation whose
 * transcript holds call [rows] while the list holds none of its calls lost
 * them to [callIndexWithoutConversation] without losing its filed keys. Its
 * keys are dropped and its rows filed. A conversation with any call in the
 * list is left alone, so a call recorded live (filed under its start time,
 * while its transcript row carries its end) is never listed twice.
 */
public fun callIndexRepaired(state: CallIndexState, rows: List<Pair<String, CallHistoryEntry>>): CallIndexState {
    val listed = state.entries.mapNotNullTo(HashSet()) { it.conversationIdHex?.lowercase() }
    val lost = rows.filter { (_, entry) ->
        val hex = entry.conversationIdHex?.lowercase()
        hex != null && hex !in listed
    }
    if (lost.isEmpty()) return state
    var cleared = state
    for (hex in lost.mapNotNullTo(LinkedHashSet()) { it.second.conversationIdHex?.lowercase() }) {
        cleared = CallIndexState(
            entries = cleared.entries,
            filed = cleared.filed.filterNotTo(LinkedHashSet()) { isFiledKeyOf(it, hex) },
        )
    }
    return callIndexFiled(cleared, lost)
}

/** [entries] in display order, capped at [max]. */
public fun callHistoryOrdered(
    entries: List<CallHistoryEntry>,
    max: Int = CALL_HISTORY_MAX,
): List<CallHistoryEntry> = entries
    .sortedWith(
        compareByDescending<CallHistoryEntry> { it.startedAtMs }
            .thenBy { it.conversationIdHex ?: "" }
            .thenBy { it.callIdHex ?: "" },
    )
    .take(max.coerceAtLeast(0))

/** A run of calls that happened on the same calendar day. */
public data class CallHistoryDay(
    /** The day, in the viewer's own zone. The grouping key, not the label. */
    val day: java.time.LocalDate,
    val rows: List<CallHistoryEntry>,
)

/**
 * Group an already-ordered list into days.
 *
 * The day is computed in the VIEWER's zone rather than UTC: a call at 00:30
 * local is today to the person who made it, and a list that files it under
 * yesterday because UTC says so is wrong in a way nobody would think to report.
 */
public fun callHistoryByDay(
    rows: List<CallHistoryEntry>,
    zone: ZoneId,
): List<CallHistoryDay> {
    val days = ArrayList<CallHistoryDay>()
    var current: MutableList<CallHistoryEntry>? = null
    var currentDay: java.time.LocalDate? = null
    for (row in rows) {
        val day = Instant.ofEpochMilli(row.startedAtMs).atZone(zone).toLocalDate()
        if (current == null || currentDay != day) {
            current = ArrayList()
            currentDay = day
            days += CallHistoryDay(day = day, rows = current)
        }
        current += row
    }
    return days
}

/**
 * Which way the row's glyph points. A declined call is drawn with the missed
 * glyph — neither ever connected — and the words say so too, so nothing rests
 * on the glyph alone.
 */
public enum class CallDirectionGlyph { OUTGOING, INCOMING, MISSED }

/** The glyph for an outcome. */
public fun callDirectionGlyph(outcome: CallOutcome): CallDirectionGlyph = when (outcome) {
    CallOutcome.OUTGOING -> CallDirectionGlyph.OUTGOING
    CallOutcome.INCOMING -> CallDirectionGlyph.INCOMING
    CallOutcome.MISSED, CallOutcome.DECLINED, CallOutcome.FAILED -> CallDirectionGlyph.MISSED
}

/**
 * Whether the row is drawn in the danger tint — the outcomes where nobody
 * spoke. Never the only signal: the label says it too.
 */
public fun callHistoryIsUnanswered(outcome: CallOutcome): Boolean =
    outcome == CallOutcome.MISSED || outcome == CallOutcome.DECLINED || outcome == CallOutcome.FAILED

/**
 * How a row of the Calls list is drawn and what it answers to
 * (`docs/ux/navigation.md` sections 5.1 and 5.1.1).
 */
public data class CallHistoryRowShape(
    /** A tap opens the conversation. `false`: the row is not a tap target. */
    val opensConversation: Boolean,
    /** A trailing button calls back in the row's mode. */
    val callBack: Boolean,
    /** The avatar is the group-call glyph, never initials. */
    val groupCallGlyph: Boolean,
)

/**
 * The shape of [entry]'s row. A call with no conversation has nothing to open
 * and cannot be called back (an ended link call cannot be joined again), so it
 * is neither a tap target nor carries a call button.
 */
public fun callHistoryRowShape(entry: CallHistoryEntry): CallHistoryRowShape =
    if (entry.hasNoConversation) {
        CallHistoryRowShape(opensConversation = false, callBack = false, groupCallGlyph = true)
    } else {
        CallHistoryRowShape(opensConversation = true, callBack = true, groupCallGlyph = false)
    }
