// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody

/**
 * The per-chat ongoing-group-call ledger: what this device believes about a
 * group call announced on a chat's MLS journal.
 *
 * ## Why this exists
 *
 * A group call used to be invisible to everyone but its initiator.
 * `ChatViewModel.startGroupCall` connected to the SFU and published nothing to
 * the group, and the receive side had nothing to consume a group-control body
 * with anyway (the 1x1 [CallSession] names them only to satisfy an exhaustive
 * `when`). The wire already carried the three group-control bodies
 * (`GroupCallStart` / `GroupCallJoin` / `GroupCallLeave`, fields 14..16 of
 * `CallSignalingPayload`) with `CallEnd` reused as the group terminal, so this
 * file adds no wire change — only the state machine that consumes them.
 *
 * ## The contract (shared with the web and iOS clients)
 *
 *  - `GroupCallStart` creates the record. When a record for a DIFFERENT call is
 *    already live, the call that stays is the lowest `(startedAtMs, callIdHex)`,
 *    so every device that observes the same two starts converges on the same
 *    call regardless of arrival order.
 *  - `GroupCallJoin` / `GroupCallLeave` update `joinedLeaves`, and only for the
 *    call the record is about. A body naming any other `call_id` is dropped
 *    (C-12).
 *  - `CallEnd` for the recorded call clears the record.
 *  - LIVE means started within the last [GROUP_CALL_MAX_AGE_MS] AND either
 *    somebody has joined or it was started within the last
 *    [GROUP_CALL_UNJOINED_GRACE_MS]. An announced call nobody connected to stops
 *    advertising itself after the grace; a call whose last leaver crashed
 *    before publishing `GroupCallLeave` stops after the maximum age.
 *
 * ## C-11
 *
 * Group-control bodies never reach the 1x1 [CallSession]: [callSignalingRoute]
 * sends them to this ledger only. `CallEnd` is the one body both paths consume,
 * and each drops it unless the `call_id` matches its own call (C-12).
 *
 * Everything here is pure and clock-free (the caller supplies every time), so
 * the host unit suite drives it without the UniFFI `.so`: the FFI enum types it
 * reads are plain Kotlin data classes.
 */

/** A group call older than this is never live, joined or not. */
internal const val GROUP_CALL_MAX_AGE_MS: Long = 4L * 60L * 60L * 1000L

/** An announced call with nobody joined stays live for this long. */
internal const val GROUP_CALL_UNJOINED_GRACE_MS: Long = 60_000L

/** One chat's recorded group call. */
internal data class GroupCallRecord(
    /** Lowercase hex of the 16-byte `call_id`. */
    val callIdHex: String,
    val audio: Boolean,
    val video: Boolean,
    val initiatorLeaf: UInt,
    /**
     * The announced `created_at_ms`, bounded above by the DS row's own
     * `created_at`. A sender can make its call look OLDER (which only shortens
     * its liveness), never newer than the moment the server saw the row.
     */
    val startedAtMs: Long,
    val joinedLeaves: Set<UInt>,
    val lastActivityMs: Long,
)

/** A group-call body reduced to what the ledger reads. */
internal sealed interface GroupCallSignal {
    val callIdHex: String

    data class Start(
        override val callIdHex: String,
        val initiatorLeaf: UInt,
        val createdAtMs: Long,
        val audio: Boolean,
        val video: Boolean,
    ) : GroupCallSignal

    data class Join(override val callIdHex: String, val leaf: UInt) : GroupCallSignal

    data class Leave(override val callIdHex: String, val leaf: UInt) : GroupCallSignal

    data class End(override val callIdHex: String) : GroupCallSignal
}

/** Where a decoded call-signaling body goes. */
internal enum class CallSignalingRoute {
    /** `GroupCallStart` / `GroupCallJoin` / `GroupCallLeave` — never the 1x1
     *  session (C-11). */
    GROUP_LEDGER_ONLY,

    /** `CallOffer` / `CallAnswer` / `IceCandidate` — the 1x1 session only. */
    ONE_TO_ONE_ONLY,

    /** `CallEnd` — terminal for both kinds; each side filters on `call_id`. */
    BOTH,
}

internal fun callSignalingRoute(body: FfiCallSignalingBody): CallSignalingRoute = when (body) {
    is FfiCallSignalingBody.GroupStart,
    is FfiCallSignalingBody.GroupJoin,
    is FfiCallSignalingBody.GroupLeave,
    -> CallSignalingRoute.GROUP_LEDGER_ONLY
    is FfiCallSignalingBody.Offer,
    is FfiCallSignalingBody.Answer,
    is FfiCallSignalingBody.IceCandidate,
    -> CallSignalingRoute.ONE_TO_ONE_ONLY
    is FfiCallSignalingBody.End -> CallSignalingRoute.BOTH
}

/** The ledger view of [body], or `null` for a body the ledger does not read. */
internal fun groupCallSignalOf(body: FfiCallSignalingBody): GroupCallSignal? = when (body) {
    is FfiCallSignalingBody.GroupStart -> GroupCallSignal.Start(
        callIdHex = callIdHexOf(body.callId),
        initiatorLeaf = body.initiatorLeaf,
        createdAtMs = ulongMillisToLong(body.createdAtMs),
        audio = body.media.audio,
        video = body.media.video,
    )
    is FfiCallSignalingBody.GroupJoin -> GroupCallSignal.Join(
        callIdHex = callIdHexOf(body.callId),
        leaf = body.leafIndex,
    )
    is FfiCallSignalingBody.GroupLeave -> GroupCallSignal.Leave(
        callIdHex = callIdHexOf(body.callId),
        leaf = body.leafIndex,
    )
    is FfiCallSignalingBody.End -> GroupCallSignal.End(callIdHexOf(body.callId))
    is FfiCallSignalingBody.Offer,
    is FfiCallSignalingBody.Answer,
    is FfiCallSignalingBody.IceCandidate,
    -> null
}

/**
 * May [signal] enter the ledger, given the MLS-authenticated sender leaf of
 * the journal row that carried it?
 *
 * `GroupCallStart.initiator_leaf` and `GroupCallJoin` / `GroupCallLeave`
 * `leaf_index` are fields the SENDER wrote, so on their own they authenticate
 * nothing: without this check any member could announce a call "started by"
 * someone else, or join and leave on another member's behalf. The MLS sender
 * leaf of the row IS authenticated, so a body is accepted only when the leaf it
 * claims equals it; a row with no member sender (`null`) is refused. Same rule
 * as the web client, no wire change.
 *
 * `CallEnd` names no leaf and is accepted, as on web: it only clears a record
 * for the `call_id` it names (C-12).
 */
internal fun groupCallSignalSpeaksForSender(signal: GroupCallSignal, senderLeaf: UInt?): Boolean =
    when (signal) {
        is GroupCallSignal.Start -> senderLeaf != null && signal.initiatorLeaf == senderLeaf
        is GroupCallSignal.Join -> senderLeaf != null && signal.leaf == senderLeaf
        is GroupCallSignal.Leave -> senderLeaf != null && signal.leaf == senderLeaf
        is GroupCallSignal.End -> true
    }

/**
 * The ledger signal a received group-control [body] may contribute, given the
 * MLS sender leaf of its row — [groupCallSignalOf] gated by
 * [groupCallSignalSpeaksForSender]. `null` for a body the ledger does not read
 * AND for one claiming a leaf that is not its sender's.
 */
internal fun groupCallSignalForRow(body: FfiCallSignalingBody, senderLeaf: UInt?): GroupCallSignal? =
    groupCallSignalOf(body)?.takeIf { groupCallSignalSpeaksForSender(it, senderLeaf) }

/** Lowercase hex, the one spelling every comparison in the ledger uses. */
internal fun callIdHexOf(callId: ByteArray): String =
    callId.joinToString("") { "%02x".format(it.toInt() and 0xff) }

/** Inverse of [callIdHexOf]; `null` on a malformed string. */
internal fun callIdBytesOf(hex: String): ByteArray? {
    if (hex.length % 2 != 0) return null
    val out = ByteArray(hex.length / 2)
    var i = 0
    while (i < hex.length) {
        val hi = Character.digit(hex[i], 16)
        val lo = Character.digit(hex[i + 1], 16)
        if (hi < 0 || lo < 0) return null
        out[i / 2] = ((hi shl 4) or lo).toByte()
        i += 2
    }
    return out
}

private fun ulongMillisToLong(value: ULong): Long =
    if (value > Long.MAX_VALUE.toULong()) Long.MAX_VALUE else value.toLong()

/** Is [record] a call a member could still join at [nowMs]? */
internal fun isGroupCallLive(record: GroupCallRecord, nowMs: Long): Boolean {
    val age = nowMs - record.startedAtMs
    if (age >= GROUP_CALL_MAX_AGE_MS) return false
    return record.joinedLeaves.isNotEmpty() || age < GROUP_CALL_UNJOINED_GRACE_MS
}

/**
 * Fold one signal into the chat's record.
 *
 * @param current the chat's record before this signal, or `null`.
 * @param observedAtMs when the signal was seen: the DS row's `created_at` for a
 *   received body, the local clock for a body this device just published (it
 *   never receives its own application messages back).
 * @param nowMs the local clock, used only to judge liveness when two different
 *   calls compete for the record.
 * @return the new record, or `null` when the chat has no recorded call.
 */
internal fun reduceGroupCall(
    current: GroupCallRecord?,
    signal: GroupCallSignal,
    observedAtMs: Long,
    nowMs: Long,
): GroupCallRecord? = when (signal) {
    is GroupCallSignal.Start -> {
        val candidate = GroupCallRecord(
            callIdHex = signal.callIdHex,
            audio = signal.audio,
            video = signal.video,
            initiatorLeaf = signal.initiatorLeaf,
            startedAtMs = minOf(signal.createdAtMs, observedAtMs),
            joinedLeaves = emptySet(),
            lastActivityMs = observedAtMs,
        )
        when {
            current == null -> candidate
            // A re-delivered start for the call already recorded changes nothing;
            // in particular it must not wipe the joined set.
            current.callIdHex == signal.callIdHex -> current
            // A dead record never blocks a new call.
            !isGroupCallLive(current, nowMs) -> candidate
            // A start that is not itself live never displaces a live call.
            !isGroupCallLive(candidate, nowMs) -> current
            // Two live calls: the lowest (startedAtMs, callIdHex) stays, on every
            // device, whichever start arrived first.
            precedes(candidate, current) -> candidate
            else -> current
        }
    }
    is GroupCallSignal.Join ->
        if (current != null && current.callIdHex == signal.callIdHex) {
            current.copy(
                joinedLeaves = current.joinedLeaves + signal.leaf,
                lastActivityMs = maxOf(current.lastActivityMs, observedAtMs),
            )
        } else {
            current
        }
    is GroupCallSignal.Leave ->
        if (current != null && current.callIdHex == signal.callIdHex) {
            current.copy(
                joinedLeaves = current.joinedLeaves - signal.leaf,
                lastActivityMs = maxOf(current.lastActivityMs, observedAtMs),
            )
        } else {
            current
        }
    is GroupCallSignal.End ->
        if (current != null && current.callIdHex == signal.callIdHex) null else current
}

private fun precedes(a: GroupCallRecord, b: GroupCallRecord): Boolean =
    if (a.startedAtMs != b.startedAtMs) {
        a.startedAtMs < b.startedAtMs
    } else {
        a.callIdHex < b.callIdHex
    }

/**
 * The call a chat should offer to JOIN, or `null`: a live record this device is
 * not already in. While any call is up on this chat the answer is `null` —
 * the in-call screen covers the chat, and a Join affordance behind it would
 * start a second call.
 */
internal fun joinableGroupCall(
    record: GroupCallRecord?,
    nowMs: Long,
    anyCallActive: Boolean,
    activeGroupCallIdHex: String?,
): GroupCallRecord? {
    if (record == null || anyCallActive) return null
    if (record.callIdHex == activeGroupCallIdHex) return null
    return record.takeIf { isGroupCallLive(it, nowMs) }
}

/** What the chat's call buttons do. */
internal enum class CallButtonAction { JOIN_LIVE_GROUP_CALL, START_GROUP_CALL, START_ONE_TO_ONE_CALL }

/**
 * A live group call on the chat is JOINED, never duplicated by a second start;
 * otherwise a group chat starts a group call and a two-member chat a 1x1 call.
 */
internal fun callButtonAction(liveRecord: GroupCallRecord?, isGroupChat: Boolean): CallButtonAction =
    when {
        liveRecord != null -> CallButtonAction.JOIN_LIVE_GROUP_CALL
        isGroupChat -> CallButtonAction.START_GROUP_CALL
        else -> CallButtonAction.START_ONE_TO_ONE_CALL
    }

/**
 * After this device's own `GroupCallLeave`, should it also publish `CallEnd`?
 * Only when the recorded call is the one it left and nobody else is recorded
 * as joined. A record about a different call (this device's start lost the
 * tie-break) carries no knowledge of who is in the call it left, so it ends
 * nothing.
 */
internal fun shouldEndGroupCallAfterLeave(
    record: GroupCallRecord?,
    leftCallIdHex: String,
    ownLeaf: UInt,
): Boolean =
    record != null &&
        record.callIdHex == leftCallIdHex &&
        (record.joinedLeaves - ownLeaf).isEmpty()
