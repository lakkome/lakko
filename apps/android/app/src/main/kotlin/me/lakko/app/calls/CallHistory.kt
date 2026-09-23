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

/** One call, as the cross-conversation index records it. */
public data class CallHistoryEntry(
    /** The conversation the call belongs to; the group id in lowercase hex. */
    val conversationIdHex: String,
    /** Which screen the row opens — the conversation's persisted kind. */
    val kind: ConversationKind,
    val outcome: CallOutcome,
    /** The call carried video. Picks the call-back mode and the row's icon. */
    val video: Boolean,
    /** When the call happened, in unix millis. The sort key. */
    val startedAtMs: Long,
    /** How long media flowed, or `null` when the call never connected. */
    val durationSecs: Long? = null,
)

/** The transcript row this index entry stands for. */
public fun CallHistoryEntry.toTranscriptEntry(): CallTranscriptEntry =
    CallTranscriptEntry(outcome = outcome, video = video, durationSecs = durationSecs)

/**
 * How many calls the index keeps. Older calls stay in their conversation, which
 * is where they were written in the first place.
 */
public const val CALL_HISTORY_MAX: Int = 300

/** The `|`-separated record version. */
private const val RECORD_VERSION = "v1"
private const val SEPARATOR = '|'

/**
 * One stored line.
 *
 * Positional and `|`-separated: a hex id, two enum names, a flag and two
 * numbers, none of which can contain a `|`, so the split is unambiguous
 * without escaping.
 */
public fun encodeCallHistoryEntry(entry: CallHistoryEntry): String = listOf(
    RECORD_VERSION,
    entry.conversationIdHex.lowercase(),
    entry.kind.name,
    entry.outcome.name,
    if (entry.video) "1" else "0",
    entry.startedAtMs.toString(),
    entry.durationSecs?.toString() ?: "",
).joinToString(SEPARATOR.toString())

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
    if (parts.size != 7) return null
    if (parts[0] != RECORD_VERSION) return null
    val id = parts[1].trim().lowercase()
    if (id.isEmpty()) return null
    val kind = ConversationKind.entries.firstOrNull { it.name == parts[2] } ?: return null
    val outcome = CallOutcome.entries.firstOrNull { it.name == parts[3] } ?: return null
    val video = when (parts[4]) {
        "1" -> true
        "0" -> false
        else -> return null
    }
    val startedAtMs = parts[5].toLongOrNull() ?: return null
    val duration = if (parts[6].isEmpty()) {
        null
    } else {
        parts[6].toLongOrNull()?.takeIf { it >= 0L } ?: return null
    }
    return CallHistoryEntry(
        conversationIdHex = id,
        kind = kind,
        outcome = outcome,
        video = video,
        startedAtMs = startedAtMs,
        durationSecs = duration,
    )
}

/**
 * The list after [entry] is added: newest first, capped at [max].
 *
 * Ties break on the conversation id so the order is total. Two calls that
 * started in the same millisecond — a pair placed by a script, two rows
 * restored from a blob that rounded — would otherwise sit in whatever order the
 * store happened to yield, and a list that reshuffles on a refresh reads as a
 * bug and is impossible to report.
 */
public fun callHistoryWith(
    existing: List<CallHistoryEntry>,
    entry: CallHistoryEntry,
    max: Int = CALL_HISTORY_MAX,
): List<CallHistoryEntry> = callHistoryOrdered(existing + entry, max)

/** [entries] in display order, capped at [max]. */
public fun callHistoryOrdered(
    entries: List<CallHistoryEntry>,
    max: Int = CALL_HISTORY_MAX,
): List<CallHistoryEntry> = entries
    .sortedWith(
        compareByDescending<CallHistoryEntry> { it.startedAtMs }
            .thenBy { it.conversationIdHex },
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
    CallOutcome.MISSED, CallOutcome.DECLINED -> CallDirectionGlyph.MISSED
}

/**
 * Whether the row is drawn in the danger tint — the outcomes where nobody
 * spoke. Never the only signal: the label says it too.
 */
public fun callHistoryIsUnanswered(outcome: CallOutcome): Boolean =
    outcome == CallOutcome.MISSED || outcome == CallOutcome.DECLINED
