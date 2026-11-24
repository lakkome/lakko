// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * ADR-0073 "Phase 2 revised", R.5 - ONE thread per conversation.
 *
 * The repository holds one record per MLS group. A conversation that was
 * continued in a successor group is several of them, and the person sees one:
 * one row in the list, and one timeline in which the older segments are
 * read-only history above the current group's messages.
 *
 * This file is that fold, as a pure function over the per-group threads and
 * the lineage ([planConversations] decides which group is the head):
 *
 *   * the folded thread IS the head's thread (its id, cursor, typing state,
 *     read watermarks, membership), so everything that acts on a thread acts
 *     on the conversation's current group;
 *   * the rows of every older held segment come first, in segment order, each
 *     marked [ChatMessage.historyOf] and given an id no row of another
 *     segment can have ([historyRowId]); DS message ids restart in every
 *     group, so an unmarked row of an older segment would be mistaken for the
 *     head's message of the same id;
 *   * the unread count is the sum over the segments.
 *
 * A history row is never an op target: its id is negative, which no DS
 * message id is. The one thing a history row still offers is Unpin
 * ([historyRowOffersUnpin]). Mirror of the web `chat/use-lineage.ts`.
 */

/** [message] of the older segment [segmentHex], [distance] segments before
 *  the head, as the folded timeline shows it. */
public fun historyRowOf(message: ChatMessage, segmentHex: String, distance: Int): ChatMessage {
    val d = maxOf(1, distance)
    return message.copy(
        messageId = if (message.messageId > 0) historyRowId(message.messageId, d) else message.messageId,
        // A row this device created keeps an id of its own, below every id a
        // stored message of that segment can map to.
        localId = if (message.localId != 0L) {
            message.localId - d.toLong() * HISTORY_ID_SPAN - HISTORY_ID_SPAN / 2
        } else {
            0L
        },
        // A quote points into its own segment: the jump follows it there.
        reply = message.reply?.let { r ->
            if (r.target > 0) r.copy(target = historyRowId(r.target, d)) else r
        },
        historyOf = segmentHex,
        historyMessageId = message.messageId,
    )
}

/**
 * One thread per conversation, in the order of the first held group of each
 * conversation in [threads]. A conversation of one group is its thread,
 * untouched.
 */
public fun foldConversations(threads: List<ChatThread>, lineage: Lineage): List<ChatThread> {
    val byHex = threads.associateBy { it.groupIdHex }
    val plans = planConversations(
        held = threads.map { HeldGroup(it.groupIdHex, active = !it.membershipEnded) },
        lineage = lineage,
    )
    return plans.mapNotNull { plan ->
        val head = byHex[plan.headHex] ?: return@mapNotNull null
        val headIndex = plan.heldSegments.indexOf(plan.headHex)
        val older = if (headIndex <= 0) emptyList() else plan.heldSegments.subList(0, headIndex)
        val headSegment = lineage.segmentIndexOf(plan.headHex)
        if (older.isEmpty()) {
            return@mapNotNull if (head.conversationHex == plan.conversationHex && head.segmentIndex == headSegment) {
                head
            } else {
                head.copy(conversationHex = plan.conversationHex, segmentIndex = headSegment)
            }
        }
        val history = ArrayList<ChatMessage>()
        var unread = head.localUnread
        var joinUnshown = head.joinRequestsUnshown
        var syncedThrough = head.syncedThrough
        older.forEachIndexed { index, hex ->
            val segment = byHex[hex] ?: return@forEachIndexed
            val distance = older.size - index
            for (m in segment.messages) history += historyRowOf(m, hex, distance)
            unread += segment.localUnread
            joinUnshown += segment.joinRequestsUnshown
            syncedThrough = syncedThrough + segment.syncedThrough
        }
        head.copy(
            messages = history + head.messages,
            localUnread = unread,
            joinRequestsUnshown = joinUnshown,
            syncedThrough = syncedThrough,
            conversationHex = plan.conversationHex,
            olderSegments = older,
            segmentIndex = headSegment,
            olderSegmentIndices = older.map { lineage.segmentIndexOf(it) },
        )
    }
}

/**
 * R.5 - where a row of the folded [thread] sits in the conversation's
 * timeline: its segment (the lineage index) and its DS message id IN that
 * segment. A history row carries both ([ChatMessage.historyOf],
 * [ChatMessage.historyMessageId]); every other row is the head's. `null` for
 * a row with no DS id (a row this device made and the server has not named).
 */
public fun timelinePositionOf(thread: ChatThread, message: ChatMessage): TimelinePosition? {
    val segmentHex = message.historyOf
    if (segmentHex == null) {
        return if (message.messageId > 0) TimelinePosition(thread.segmentIndex, message.messageId) else null
    }
    if (message.historyMessageId <= 0) return null
    val at = thread.olderSegments.indexOfFirst { it.equals(segmentHex, ignoreCase = true) }
    // A folded thread built before the indices existed, or by a test: the
    // held order, which is the lineage order when every segment is held.
    val segment = thread.olderSegmentIndices.getOrNull(at) ?: at.takeIf { it >= 0 } ?: return null
    return TimelinePosition(segment, message.historyMessageId)
}

/**
 * R.5 - whether a row of the folded [thread] is PAST the read position, that
 * is, not read yet as the cross-device cursor knows it
 * ([readUpToInSegment]).
 *
 * | the position is in ...             | the row is unread |
 * |------------------------------------|-------------------|
 * | an earlier segment than the row's  | yes               |
 * | the row's segment                  | id > position's   |
 * | a later segment                    | no                |
 *
 * So an unread row of an OLDER segment counts until the position passes it,
 * and a head row is never read by a position that only reached the old
 * group. A row with no timeline position is not counted.
 */
public fun rowPastReadPosition(thread: ChatThread, message: ChatMessage, position: TimelinePosition): Boolean {
    val at = timelinePositionOf(thread, message) ?: return false
    return when (val read = readUpToInSegment(position, at.segment)) {
        SegmentRead.Nothing -> true
        SegmentRead.All -> false
        is SegmentRead.UpTo -> at.messageId > read.messageId
    }
}

/**
 * `docs/ux/second-device.md` section 2, "History is not news": whether a row
 * of [thread] came with the pairing history, at or below its segment's floor
 * ([ChatThread.syncedThrough]). Such a row is never unread on this device,
 * whatever the read position says. DS ids restart in every group, so a row is
 * judged by its own segment's floor and its id in that segment.
 */
public fun rowInPairingHistory(thread: ChatThread, message: ChatMessage): Boolean {
    val segment = message.historyOf ?: thread.groupIdHex
    val floor = thread.syncedThrough[segment.lowercase()] ?: return false
    val id = if (message.historyOf != null) message.historyMessageId else message.messageId
    return id in 1..floor
}

/** The folded thread a group id (of any held segment) belongs to. */
public fun conversationThreadOf(conversations: List<ChatThread>, groupIdHex: String): ChatThread? {
    val key = groupIdHex.lowercase()
    return conversations.firstOrNull { it.groupIdHex.equals(key, ignoreCase = true) }
        ?: conversations.firstOrNull { c ->
            c.conversationHex.equals(key, ignoreCase = true) || c.olderSegments.any { it.equals(key, ignoreCase = true) }
        }
}

/**
 * R.5 - the groups to delete when the person deletes a conversation, oldest
 * first and the head last: every segment this device holds. Mirror of the
 * web `conversationDeleteOrder`.
 */
public fun conversationDeleteOrder(thread: ChatThread): List<String> = thread.olderSegments + thread.groupIdHex

/**
 * R.5 / ADR-0052 decision 2 - the on-screen row a forward's origin jumps to
 * when its origin group is a segment of the conversation [thread] shows: the
 * message id itself for the head, the history id for an older segment, `null`
 * when the origin is another conversation or a segment this device does not
 * hold. Mirror of the web `forwardJumpRow`.
 */
public fun forwardJumpRow(thread: ChatThread, originGroupIdHex: String, originMessageId: Long): Long? {
    if (originMessageId <= 0) return null
    if (thread.groupIdHex.equals(originGroupIdHex, ignoreCase = true)) return originMessageId
    val index = thread.olderSegments.indexOfFirst { it.equals(originGroupIdHex, ignoreCase = true) }
    if (index < 0) return null
    return historyRowId(originMessageId, thread.olderSegments.size - index)
}
