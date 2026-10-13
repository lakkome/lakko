// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * ADR-0073 R.5 - a conversation's pins span its segments. The pin store stays
 * keyed on `(group, message id)` and is never rewritten when a conversation
 * continues in a successor group, so a pin of an older segment is still held
 * under the older group. Mirror of the segment functions of
 * `apps/web/src/chat/pin-bar.ts`.
 */

/** The pins of ONE segment (group) of a conversation: message id -> value. */
public data class SegmentPins<S, P>(
    /** How many segments this one is before the conversation's current group
     *  (`0` = the current group). */
    val distanceFromHead: Int,
    val shared: Map<Long, S>,
    val personal: Map<Long, P>,
)

/** Every held segment's pins keyed on the ON-SCREEN row id. */
public data class FoldedPins<S, P>(val shared: Map<Long, S>, val personal: Map<Long, P>)

/**
 * Fold every held segment's pins into ONE pair of maps keyed on the on-screen
 * row id: the message id itself for the current group, [historyRowId] for an
 * older one, which is the id the timeline draws that row under and the id a
 * tap jumps to. So a pin of an older segment stays in the bar and opens where
 * it was.
 */
public fun <S, P> pinsAcrossSegments(segments: List<SegmentPins<S, P>>): FoldedPins<S, P> {
    val shared = LinkedHashMap<Long, S>()
    val personal = LinkedHashMap<Long, P>()
    for (segment in segments) {
        val d = maxOf(0, segment.distanceFromHead)
        val key = { messageId: Long -> if (d == 0) messageId else historyRowId(messageId, d) }
        for ((id, pin) in segment.shared) shared[key(id)] = pin
        for ((id, at) in segment.personal) personal[key(id)] = at
    }
    return FoldedPins(shared, personal)
}

/** The stored pin an on-screen row id stands for. */
public data class PinTarget(
    /** The group the pin is stored under (hex). */
    val groupIdHex: String,
    /** The message id in THAT group. */
    val messageId: Long,
    /** Whether that group is an older segment of the conversation. */
    val older: Boolean,
)

/**
 * The inverse of [pinsAcrossSegments]: which stored `(group, message id)` an
 * on-screen row id is. [olderGroupIdsHex] is the conversation's older
 * segments, oldest first (distance `size - index` from the head). `null` for
 * a history id whose segment this device does not hold.
 */
public fun pinTargetOfRow(rowId: Long, headGroupIdHex: String, olderGroupIdsHex: List<String>): PinTarget? {
    if (rowId >= 0) return PinTarget(headGroupIdHex, rowId, older = false)
    val origin = historyRowOrigin(rowId) ?: return null
    val group = olderGroupIdsHex.getOrNull(olderGroupIdsHex.size - origin.distanceFromHead) ?: return null
    return PinTarget(group, origin.messageId, older = true)
}

/** How an unpin (or a pin) of one scope is carried out. */
public enum class PinRoute {
    /** The ordinary path: a shared change is sent to the group. */
    ORDINARY,

    /** Applied on this device only: nothing is published. */
    LOCAL_ONLY,

    /** Not offered. */
    REFUSED,
}

/**
 * A pin of an OLDER segment can be removed, and nothing else can be done to
 * it: an older segment is read-only on the wire, so its shared unpin is
 * recorded on this device alone, with a fresh stamp so a late re-send of the
 * old pin cannot bring it back.
 *
 * | segment | change | scope    | route        |
 * |---------|--------|----------|--------------|
 * | current | any    | any      | `ORDINARY`   |
 * | older   | unpin  | personal | `ORDINARY`   |
 * | older   | unpin  | shared   | `LOCAL_ONLY` |
 * | older   | pin    | any      | `REFUSED`    |
 */
public fun pinRoute(older: Boolean, pinned: Boolean, shared: Boolean): PinRoute = when {
    !older -> PinRoute.ORDINARY
    pinned -> PinRoute.REFUSED
    shared -> PinRoute.LOCAL_ONLY
    else -> PinRoute.ORDINARY
}

/**
 * What the action menu of a row of an OLDER segment offers: the `Unpin` row
 * alone when the row is pinned, and nothing otherwise (a history row takes no
 * edit, delete, reaction, reply, forward or new pin).
 */
public fun historyRowOffersUnpin(isHistoryRow: Boolean, rowIsPinned: Boolean): Boolean = isHistoryRow && rowIsPinned
