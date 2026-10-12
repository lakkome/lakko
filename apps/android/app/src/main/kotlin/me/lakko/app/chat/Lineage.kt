// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.nio.ByteBuffer

/*
 * ADR-0065 "Phase 2 revised", R.5 - one conversation over several groups.
 *
 * A conversation that cannot be repaired in place continues in a NEW MLS
 * group. The person sees one conversation: the same row, the history above,
 * the same pin / mute / archive / folder / draft / read position. That is a
 * stable CONVERSATION ID with a LINEAGE of groups, not a re-key of any store:
 *
 *   * the conversation id is the group id of the lineage's FIRST group (the
 *     root). Everything a person attaches to a conversation is keyed on it
 *     and never rewritten;
 *   * the segments are the lineage's groups in order. The message log stays
 *     per group; the timeline is the segments concatenated, ordered by
 *     `(segment index, DS message id)`, since a successor's ids restart;
 *   * everything that belongs to one MLS session stays per group. "The
 *     conversation's current group" is the lineage head: sends, calls and
 *     the push subscription use it.
 *
 * The lineage itself is the write-once register `successor group id -> root
 * conversation id` of the ADR-0058 synced state (`lakko-sync`), read here as
 * plain values. Ids are lower-case hex. Mirror of
 * `apps/web/src/chat/lineage.ts` (R.12).
 */

/** One lineage register, as projected from the synced state. */
public data class LineageEntry(
    /** Hex of the group that continues the conversation. */
    val successorHex: String,
    /** Hex of the conversation id (the lineage's first group). */
    val rootHex: String,
    /** The HLC stamp the register was written at: the only thing that
     *  carries segment order (R.13a). */
    val hlc: ULong,
)

/** The lineage of every conversation of the account. */
public class Lineage internal constructor(
    private val rootOfSuccessor: Map<String, String>,
    private val segments: Map<String, List<String>>,
) {
    /** The conversation a group belongs to: its root, or itself. */
    public fun conversationOf(groupHex: String): String {
        var current = groupHex
        val seen = HashSet<String>().apply { add(current) }
        while (true) {
            val next = rootOfSuccessor[current] ?: return current
            // A cycle names no root: the group stands alone.
            if (!seen.add(next)) return groupHex
            current = next
        }
    }

    /** A conversation's groups in order: the root, then its successors by
     *  stamp (ties broken on the successor id, so every device agrees). */
    public fun segmentsOf(conversationHex: String): List<String> =
        segments[conversationHex] ?: listOf(conversationHex)

    /** The index of a group in its conversation's segments. */
    public fun segmentIndexOf(groupHex: String): Int =
        segmentsOf(conversationOf(groupHex)).indexOf(groupHex).coerceAtLeast(0)

    /** Whether a group is some conversation's successor (not a root). */
    public fun isSuccessor(groupHex: String): Boolean = conversationOf(groupHex) != groupHex
}

/**
 * Build the lineage from the registers. A register whose root is itself a
 * successor is followed to the real root, so a chain still reads as one
 * conversation; a register naming itself, and a cycle, are ignored.
 */
public fun buildLineage(entries: List<LineageEntry>): Lineage {
    val rootOfSuccessor = LinkedHashMap<String, String>()
    val stampOf = HashMap<String, ULong>()
    for (e in entries) {
        if (e.successorHex == e.rootHex) continue
        rootOfSuccessor[e.successorHex] = e.rootHex
        stampOf[e.successorHex] = e.hlc
    }
    val resolver = Lineage(rootOfSuccessor, emptyMap())
    val lists = LinkedHashMap<String, MutableList<String>>()
    for (successorHex in rootOfSuccessor.keys) {
        val root = resolver.conversationOf(successorHex)
        if (root == successorHex) continue
        lists.getOrPut(root) { ArrayList() }.add(successorHex)
    }
    val segments = LinkedHashMap<String, List<String>>()
    for ((root, list) in lists) {
        list.sortWith(compareBy<String> { stampOf[it] ?: 0UL }.thenBy { it })
        segments[root] = listOf(root) + list
    }
    return Lineage(rootOfSuccessor, segments)
}

/** The empty lineage: every group is its own conversation. */
public val NO_LINEAGE: Lineage = buildLineage(emptyList())

/** This device's standing in one segment. */
public enum class SegmentMembership { ACTIVE, ENDED, ABSENT }

/**
 * "The conversation's current group" (R.5): the LAST segment this device is
 * an active member of. A successor the device has not joined yet is not the
 * head for it: it keeps sending where it is (R.7 case 3) or holds the send
 * (case 1). `null` when the device holds no segment at all. When no segment
 * is active, the last segment the device holds is the head, so the row and
 * its history stay reachable.
 */
public fun headSegment(segments: List<String>, membership: (String) -> SegmentMembership): String? {
    var lastHeld: String? = null
    var lastActive: String? = null
    for (hex in segments) {
        val m = membership(hex)
        if (m == SegmentMembership.ABSENT) continue
        lastHeld = hex
        if (m == SegmentMembership.ACTIVE) lastActive = hex
    }
    return lastActive ?: lastHeld
}

/** A position in a conversation's timeline. */
public data class TimelinePosition(val segment: Int, val messageId: Long)

/** Timeline order (R.5): `(segment index, DS message id)`. */
public fun compareTimeline(a: TimelinePosition, b: TimelinePosition): Int =
    if (a.segment != b.segment) a.segment.compareTo(b.segment) else a.messageId.compareTo(b.messageId)

/**
 * The cross-device read cursor of a conversation is one register keyed on the
 * conversation id. Its value has always been the 8-byte big-endian DS message
 * id; a successor's ids restart, so a position past the first segment also
 * carries the segment:
 *
 *   * segment 0: `message_id(8 BE)`: unchanged, what every build writes;
 *   * segment n > 0: `message_id(8 BE) || segment(4 BE)`.
 *
 * Byte-identical across the three clients, since the cursor is synced. A
 * reader that knows only the 8-byte form reads a position in a later segment
 * as "that message id in the first segment", which can only make it show
 * LESS as read than was read.
 */
public fun encodeReadPosition(position: TimelinePosition): ByteArray {
    val withSegment = position.segment > 0
    val out = ByteBuffer.allocate(if (withSegment) 12 else 8)
    out.putLong(maxOf(0L, position.messageId))
    if (withSegment) out.putInt(position.segment)
    return out.array()
}

/** Decode a read cursor's bytes; `null` when shorter than a message id. */
public fun decodeReadPosition(bytes: ByteArray): TimelinePosition? {
    if (bytes.size < 8) return null
    val buf = ByteBuffer.wrap(bytes)
    val id = buf.getLong()
    // An id above the 53-bit range the web client can hold is not a position
    // any client wrote (and a negative `Long` is the top of the u64 range).
    if (id < 0 || id > MAX_SAFE_MESSAGE_ID) return null
    val segment = if (bytes.size >= 12) buf.getInt() else 0
    if (segment < 0) return null
    return TimelinePosition(segment, id)
}

private const val MAX_SAFE_MESSAGE_ID: Long = 9_007_199_254_740_991L

/** How far one segment is read. */
public sealed interface SegmentRead {
    /** Nothing of this segment is read. */
    public data object Nothing : SegmentRead

    /** Everything in this segment is read. */
    public data object All : SegmentRead

    /** Read up to this message id of the segment. */
    public data class UpTo(val messageId: Long) : SegmentRead
}

/**
 * What the read position means for ONE segment's own message ids.
 *
 * | the position is in ... | this segment is read up to |
 * |------------------------|----------------------------|
 * | an earlier segment     | nothing                    |
 * | this segment           | the position's message id  |
 * | a later segment        | everything                 |
 */
public fun readUpToInSegment(position: TimelinePosition?, segment: Int): SegmentRead = when {
    position == null -> SegmentRead.Nothing
    position.segment < segment -> SegmentRead.Nothing
    position.segment > segment -> SegmentRead.All
    else -> SegmentRead.UpTo(position.messageId)
}

/** A group this device holds, as the list planning reads it. */
public data class HeldGroup(val groupHex: String, val active: Boolean)

/** One conversation of the list. */
public data class ConversationPlan(
    /** The conversation id (the root's hex): the row's identity, and the key
     *  of every person-facing store. */
    val conversationHex: String,
    /** The group the row opens and sends into. */
    val headHex: String,
    /** Every segment this device holds, in timeline order. */
    val heldSegments: List<String>,
)

/**
 * One row per conversation (R.5). Groups the device holds are folded by
 * conversation; the order of the result follows the first held group of each
 * conversation in [held].
 */
public fun planConversations(held: List<HeldGroup>, lineage: Lineage): List<ConversationPlan> {
    val byHex = held.associateBy { it.groupHex }
    val out = ArrayList<ConversationPlan>()
    val seen = HashSet<String>()
    for (g in held) {
        val conversationHex = lineage.conversationOf(g.groupHex)
        if (!seen.add(conversationHex)) continue
        val segments = lineage.segmentsOf(conversationHex)
        val heldSegments = segments.filter { it in byHex }
        val headHex = headSegment(segments) { hex ->
            val entry = byHex[hex]
            when {
                entry == null -> SegmentMembership.ABSENT
                entry.active -> SegmentMembership.ACTIVE
                else -> SegmentMembership.ENDED
            }
        } ?: continue
        out += ConversationPlan(conversationHex, headHex, heldSegments)
    }
    return out
}

/** The DS journal retention (30 days today): how long an old group's journal
 *  is still read after the move. Chosen for parity with the DS, not measured. */
public const val OLD_SEGMENT_POLL_WINDOW_MS: Long = 30L * 24 * 60 * 60 * 1000

/**
 * R.5 "the old segment after the move": whether an old segment's journal is
 * still polled.
 *
 * | the group is ...                                  | polled |
 * |---------------------------------------------------|--------|
 * | the conversation's head                           | yes    |
 * | an older segment of a ONE-TO-ONE                  | no     |
 * | an older segment of a group, inside the retention | yes    |
 * | an older segment of a group, past the retention   | no     |
 */
public fun oldSegmentPolled(
    isHead: Boolean,
    oneToOne: Boolean,
    /** When this device learned the conversation moved, UNIX ms; `null` when
     *  unknown (then the window has not started). */
    movedAtMs: Long?,
    nowMs: Long,
): Boolean {
    if (isHead) return true
    if (oneToOne) return false
    if (movedAtMs == null) return true
    return nowMs - movedAtMs < OLD_SEGMENT_POLL_WINDOW_MS
}

/**
 * R.5 "push routing ... the old group's subscription is retired": whether
 * this device retires its push subscription for one group now.
 *
 * | the group is ...                                             | retires |
 * |--------------------------------------------------------------|---------|
 * | its own conversation, or the conversation's current group    | no      |
 * | an older segment, and the current group's membership is live | yes     |
 * | an older segment, the current group not live here            | no      |
 * | an older segment already retired                             | no      |
 */
public fun retiresOldSegmentPush(
    groupHex: String,
    /** The head of the group's conversation on this device, or `null`. */
    headHex: String?,
    headActive: Boolean,
    alreadyRetired: Boolean,
): Boolean {
    if (alreadyRetired) return false
    if (headHex == null || headHex == groupHex) return false
    return headActive
}

/**
 * A successor's DS message ids restart, so the rows of an OLDER segment
 * cannot share the head's id space on screen. An older segment's row is shown
 * under `id - distance * HISTORY_ID_SPAN` (distance >= 1 from the head),
 * which is negative, unique per `(segment, message id)`, ascending inside a
 * segment and across segments. Never stored and never sent: a history row is
 * read-only.
 */
public const val HISTORY_ID_SPAN: Long = 1L shl 40

/** The on-screen id of an older segment's row. */
public fun historyRowId(messageId: Long, distanceFromHead: Int): Long =
    messageId - maxOf(1, distanceFromHead).toLong() * HISTORY_ID_SPAN

/** Whether an on-screen id belongs to an older segment (read-only). */
public fun isHistoryRowId(id: Long?): Boolean = id != null && id < 0

/** The message id and the segment distance behind a history row id. */
public data class HistoryRowOrigin(val messageId: Long, val distanceFromHead: Int)

/**
 * The inverse of [historyRowId]: the message id and the segment distance an
 * on-screen history id stands for, or `null` for an id that is not one (a
 * head row, or a number no history row can have).
 */
public fun historyRowOrigin(rowId: Long): HistoryRowOrigin? {
    if (rowId >= 0) return null
    val distance = (-rowId + HISTORY_ID_SPAN - 1) / HISTORY_ID_SPAN
    if (distance > Int.MAX_VALUE) return null
    val messageId = rowId + distance * HISTORY_ID_SPAN
    if (messageId <= 0 || messageId >= HISTORY_ID_SPAN) return null
    return HistoryRowOrigin(messageId, distance.toInt())
}

// ---- the person-facing stores, read through any segment -----------------

/**
 * A store keyed by conversation id, as the screens read it: every segment of
 * a conversation answers with the conversation's value, so a caller that
 * holds the id of ANY of its groups (the head a chat screen is open on, an
 * older segment a notification names) reads the same row. A value stored
 * under a successor's own id (written by a build that did not know the
 * lineage yet) is shadowed by the conversation's.
 *
 * [project] turns the conversation's value into what one segment sees; it
 * returns `null` when that segment has no value.
 */
public fun <V> aliasAcrossSegments(
    byConversation: Map<String, V>,
    entries: List<LineageEntry>,
    lineage: Lineage,
    project: (value: V, segment: Int) -> V? = { v, _ -> v },
): Map<String, V> {
    if (entries.isEmpty()) return byConversation
    val out = LinkedHashMap<String, V>(byConversation.size + entries.size)
    val successors = entries.mapTo(HashSet()) { it.successorHex }
    for ((key, value) in byConversation) {
        if (key in successors && lineage.conversationOf(key) != key) continue
        project(value, 0)?.let { out[key] = it }
    }
    for (successorHex in successors) {
        val root = lineage.conversationOf(successorHex)
        if (root == successorHex) continue
        val value = byConversation[root] ?: continue
        project(value, lineage.segmentIndexOf(successorHex))?.let { out[successorHex] = it }
    }
    return out
}

/**
 * The read cursor of a conversation as ONE segment's own 8-byte message id:
 * what every reader of a cursor on this client decodes ([readUpToInSegment]).
 * `null` when nothing of the segment is read. "Everything" is the largest id.
 */
public fun readCursorBytesForSegment(conversationCursor: ByteArray, segment: Int): ByteArray? =
    when (val read = readUpToInSegment(decodeReadPosition(conversationCursor), segment)) {
        SegmentRead.Nothing -> null
        SegmentRead.All -> ByteBuffer.allocate(8).putLong(Long.MAX_VALUE).array()
        is SegmentRead.UpTo -> ByteBuffer.allocate(8).putLong(read.messageId).array()
    }

/**
 * The bytes a read cursor is WRITTEN with when a screen marks [segment] of a
 * conversation read up to the 8-byte id [segmentCursor]; `null` when the
 * write is dropped: malformed bytes, or a position that is not past the one
 * the conversation already holds ([held], the stored bytes). A cursor never
 * moves back across segments: marking an older segment read after the head
 * was read changes nothing.
 */
public fun readCursorBytesForWrite(segmentCursor: ByteArray, segment: Int, held: ByteArray?): ByteArray? {
    if (segmentCursor.size < 8) return null
    val id = ByteBuffer.wrap(segmentCursor).getLong()
    if (id < 0 || id > MAX_SAFE_MESSAGE_ID) return null
    val next = TimelinePosition(maxOf(0, segment), id)
    val current = held?.let(::decodeReadPosition)
    if (current != null && current.segment != next.segment && compareTimeline(next, current) <= 0) return null
    return encodeReadPosition(next)
}
