// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

/**
 * What the feed has shown the person (ADR-0063 section 5), per group. Mirror of
 * `apps/web/src/feed/feed-seen.ts`.
 *
 * A group's seen set is every message at or below its watermark plus the ids
 * in `feedSeen`, the messages the feed kept fully on screen for
 * [FEED_SEEN_DWELL_MS]. On Android the watermark is the larger of the
 * cross-device read cursor (the chat screen moves it to the highest message
 * it holds, which is what "viewed" always meant here) and the feed's own
 * persisted watermark (`PersistedGroup.feedSeenThrough`), which exists because
 * the read cursor is only written when a DeviceGroup is running. The person's
 * own and system rows count as seen. When the seen set becomes contiguous past
 * the watermark — `contiguousSeen` in the core, the one definition every
 * client uses — the watermark moves up to it, the ids below it are dropped
 * from `feedSeen`, and the caller sends the group a read receipt and the other
 * devices a read cursor at the new watermark. It never moves past a message
 * the person did not see.
 *
 * Pure: the log write lives in `ChatRepository.recordFeedSeen`, the sends in
 * the Feed screen.
 */
public const val FEED_SEEN_DWELL_MS: Long = 2_000L

/** One message of a group as the seen rule reads it. A [deleted] message
 *  counts as seen (`docs/ux/message-actions.md` section 9): it is never
 *  shown, so it must never hold back how far the conversation is read. */
public data class SeenRow(val messageId: Long, val direction: String, val deleted: Boolean = false)

/** A group's seen state. */
public data class GroupSeenState(
    /** Everything at or below is seen; `null` when nothing is. */
    val through: Long?,
    /** Seen above [through], out of order. */
    val feedSeen: Set<Long>,
)

/** `true` when the person has seen [messageId] ([direction] of the row). */
public fun isSeen(state: GroupSeenState, messageId: Long, direction: String): Boolean {
    if (direction != "incoming") return true
    if (messageId <= 0) return false
    val through = state.through
    if (through != null && messageId <= through) return true
    return messageId in state.feedSeen
}

public data class MarkSeenResult(
    val state: GroupSeenState,
    /** The new watermark when it moved past [GroupSeenState.through], else `null`. */
    val advancedTo: Long?,
    /** How many incoming ids newly joined the seen set (for the local unread count). */
    val newlySeen: Int,
)

/**
 * Add [ids] to the group's seen set and move the watermark as far as the set is
 * contiguous. Ids that name no message in [rows] are ignored.
 */
public fun markSeen(
    rows: List<SeenRow>,
    state: GroupSeenState,
    ids: Collection<Long>,
    contiguous: (ids: List<Long>, seen: List<Long>) -> Long?,
): MarkSeenResult {
    val present = rows.filter { it.messageId > 0 }.associateBy { it.messageId }
    val through = state.through
    val feedSeen = state.feedSeen.filterTo(LinkedHashSet()) { it in present }
    var newlySeen = 0
    for (id in ids) {
        val row = present[id] ?: continue
        if (row.direction == "incoming" && !isSeen(state, id, row.direction) && feedSeen.add(id)) {
            newlySeen += 1
        } else {
            feedSeen.add(id)
        }
    }
    val all = ArrayList<Long>()
    val seen = ArrayList<Long>()
    for (r in rows) {
        if (r.messageId <= 0) continue
        all += r.messageId
        val isSeenRow = r.deleted || r.direction != "incoming" ||
            (through != null && r.messageId <= through) ||
            r.messageId in feedSeen
        if (isSeenRow) seen += r.messageId
    }
    val watermark = contiguous(all, seen)
    val advancedTo = watermark?.takeIf { through == null || it > through }
    val newThrough = advancedTo ?: through
    val kept = feedSeen.filterTo(LinkedHashSet()) { newThrough == null || it > newThrough }
    return MarkSeenResult(GroupSeenState(newThrough, kept), advancedTo, newlySeen)
}

/** The 8-byte big-endian form a DeviceGroup read cursor carries. */
public fun readCursorBytes(id: Long): ByteArray {
    val out = ByteArray(8)
    var v = id
    for (i in 7 downTo 0) {
        out[i] = (v and 0xff).toByte()
        v = v ushr 8
    }
    return out
}

/** Inverse of [readCursorBytes]; `null` for a short array. */
public fun readCursorId(bytes: ByteArray?): Long? {
    if (bytes == null || bytes.size < 8) return null
    var v = 0L
    for (i in 0 until 8) v = (v shl 8) or (bytes[i].toLong() and 0xff)
    return v
}
