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
 *  - `GroupCallJoin` / `GroupCallLeave` update `joinedMembers`, and only for
 *    the call the record is about. A body naming any other `call_id` is dropped
 *    (C-12). A participant re-publishes its `GroupCallJoin` every
 *    [GROUP_CALL_HEARTBEAT_MS] while it is in the call, so a Join is both
 *    "I joined" and "I am still here".
 *  - `joinedMembers` is keyed on the sender leaf's own (device) signature key,
 *    resolved from the MLS sender leaf AT RECEIPT
 *    (`chat/AuthorIdentity.kt` `resolveLeafSignatureKey`, the same moment
 *    message authorship is resolved), never on a leaf index: OpenMLS hands a vacated
 *    leaf to the next member added, and a leaf-keyed set would let that member
 *    inherit a departed participant's place in the call (C-68). A Join / Leave
 *    whose sender could not be resolved is dropped. The value is the DS row
 *    time of that member's newest Join; a Join never lowers it, a Leave removes
 *    the member.
 *  - `CallEnd` for the recorded call clears the record.
 *  - A joined member counts only while its newest Join is at most
 *    [GROUP_CALL_MEMBER_FRESH_MS] old ([freshGroupCallMembers]). A participant
 *    whose app was killed mid-call never publishes a Leave; it stops counting
 *    once its heartbeats stop arriving.
 *  - LIVE means started within the last [GROUP_CALL_MAX_AGE_MS] AND either at
 *    least one member is fresh or it was started within the last
 *    [GROUP_CALL_UNJOINED_GRACE_MS].
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

/** A participant re-publishes its `GroupCallJoin` this often while in the call. */
public const val GROUP_CALL_HEARTBEAT_MS: Long = 60_000L

/**
 * A joined member counts for this long after its newest Join: two heartbeat
 * intervals (one lost heartbeat is tolerated) plus a poll's worth of delivery
 * slack. Identical on web, iOS and Android.
 */
internal const val GROUP_CALL_MEMBER_FRESH_MS: Long = 150_000L

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
    /**
     * Device signature key (base64, resolved at receipt) -> that device's
     * person key and the DS row time of its newest Join / heartbeat. Includes
     * devices that are no longer fresh; read it through [freshGroupCallMembers].
     */
    val joinedMembers: Map<String, GroupCallMember>,
    val lastActivityMs: Long,
    /**
     * The person (base64 account key) whose device announced the call,
     * resolved from the MLS sender of the `GroupCallStart` at receipt (C-68);
     * for a call this device started, its own person key. `null` when it could
     * not be resolved, and for a record stored before it was recorded. Only
     * this person may end the call for everyone (`docs/ux/calls.md` section
     * 4.1.2), and an unknown starter can never do so.
     */
    val initiatorPersonKey: String? = null,
)

/** One joined device of a [GroupCallRecord]. */
internal data class GroupCallMember(
    /** The person (account) key the sender leaf resolved to at receipt, the
     *  same resolution authorship uses; the device key when that did not
     *  resolve, so the device still counts as someone. */
    val personKey: String,
    val lastSeenMs: Long,
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
        /** The sender's person key resolved at receipt; `null` until attached
         *  with [withSender]. Never the leaf the body names. */
        val initiatorPersonKey: String? = null,
    ) : GroupCallSignal

    /**
     * @property leaf the leaf the body names (checked against the row's MLS
     *   sender leaf by [groupCallSignalSpeaksForSender]).
     * @property memberKey the sender leaf's own (device) signature key resolved
     *   at receipt; `null` until attached with [withSender], and a Join without
     *   one changes nothing.
     * @property personKey the sender's person key resolved at receipt; `null`
     *   falls back to [memberKey].
     */
    data class Join(
        override val callIdHex: String,
        val leaf: UInt,
        val memberKey: String? = null,
        val personKey: String? = null,
    ) : GroupCallSignal

    /** See [Join]. A Leave removes the device named by [memberKey]. */
    data class Leave(
        override val callIdHex: String,
        val leaf: UInt,
        val memberKey: String? = null,
        val personKey: String? = null,
    ) : GroupCallSignal

    data class End(override val callIdHex: String) : GroupCallSignal
}

/**
 * [this] with the sender identity attached: the device and the person on a
 * Join / Leave, the person on a Start (the device key when the person did not
 * resolve, so the same device is still recognised). End carries none.
 */
internal fun GroupCallSignal.withSender(memberKey: String?, personKey: String?): GroupCallSignal = when (this) {
    is GroupCallSignal.Join -> copy(memberKey = memberKey, personKey = personKey)
    is GroupCallSignal.Leave -> copy(memberKey = memberKey, personKey = personKey)
    is GroupCallSignal.Start -> copy(initiatorPersonKey = (personKey ?: memberKey)?.takeIf { it.isNotEmpty() })
    is GroupCallSignal.End -> this
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

/**
 * The frame's kind as one stable word, for the routing log line.
 *
 * Only the kind, never a field: the point of that line is to tell "the call
 * frame never arrived" from "it arrived and was dropped", and the kind is all
 * that distinguishes an offer worth ringing for from the candidates behind it.
 */
internal fun callSignalingBodyKindName(body: FfiCallSignalingBody): String = when (body) {
    is FfiCallSignalingBody.Offer -> "offer"
    is FfiCallSignalingBody.Answer -> "answer"
    is FfiCallSignalingBody.IceCandidate -> "ice"
    is FfiCallSignalingBody.End -> "end"
    is FfiCallSignalingBody.GroupStart -> "group-start"
    is FfiCallSignalingBody.GroupJoin -> "group-join"
    is FfiCallSignalingBody.GroupLeave -> "group-leave"
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
 * MLS sender leaf of its row and the identity that leaf resolved to at receipt
 * — [groupCallSignalOf] gated by [groupCallSignalSpeaksForSender], with
 * [senderKey] attached. `null` for a body the ledger does not read AND for one
 * claiming a leaf that is not its sender's.
 */
internal fun groupCallSignalForRow(
    body: FfiCallSignalingBody,
    senderLeaf: UInt?,
    senderKey: String? = null,
    senderPersonKey: String? = null,
): GroupCallSignal? =
    groupCallSignalOf(body)
        ?.takeIf { groupCallSignalSpeaksForSender(it, senderLeaf) }
        ?.withSender(senderKey, senderPersonKey)

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

/**
 * The device keys of [record] whose newest Join is at most
 * [GROUP_CALL_MEMBER_FRESH_MS] old at [nowMs], minus [ignoreDeviceKey]. A row
 * time ahead of the local clock (skew) counts as fresh.
 *
 * @param ignoreDeviceKey this device's own key when it is NOT in the call
 *   ([groupCallIgnoredDeviceKey]): an entry left by this device's own session
 *   before a relaunch is a dead session, never a participant.
 */
internal fun freshGroupCallMembers(
    record: GroupCallRecord,
    nowMs: Long,
    ignoreDeviceKey: String? = null,
): Set<String> =
    record.joinedMembers
        .filter { (key, member) ->
            key != ignoreDeviceKey && nowMs - member.lastSeenMs <= GROUP_CALL_MEMBER_FRESH_MS
        }
        .keys

/** "N people in the call": DISTINCT person keys among the fresh devices, so two
 *  devices of one account count once. */
internal fun freshGroupCallPeopleCount(
    record: GroupCallRecord,
    nowMs: Long,
    ignoreDeviceKey: String? = null,
): Int =
    freshGroupCallMembers(record, nowMs, ignoreDeviceKey)
        .mapNotNull { record.joinedMembers[it]?.personKey }
        .toSet()
        .size

/**
 * The device key a liveness / count read of [record] must ignore: this
 * device's own [ownDeviceKey] unless this device is currently in that very
 * call ([activeGroupCallIdHex] is the host slot's call id).
 */
internal fun groupCallIgnoredDeviceKey(
    record: GroupCallRecord,
    ownDeviceKey: String?,
    activeGroupCallIdHex: String?,
): String? = if (activeGroupCallIdHex == record.callIdHex) null else ownDeviceKey

/** Is [record] a call a member could still join at [nowMs]? */
internal fun isGroupCallLive(record: GroupCallRecord, nowMs: Long, ignoreDeviceKey: String? = null): Boolean {
    val age = nowMs - record.startedAtMs
    if (age >= GROUP_CALL_MAX_AGE_MS) return false
    return age < GROUP_CALL_UNJOINED_GRACE_MS ||
        freshGroupCallMembers(record, nowMs, ignoreDeviceKey).isNotEmpty()
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
            joinedMembers = emptyMap(),
            lastActivityMs = observedAtMs,
            initiatorPersonKey = signal.initiatorPersonKey,
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
    is GroupCallSignal.Join -> {
        val key = signal.memberKey
        if (current != null && current.callIdHex == signal.callIdHex && key != null) {
            val previous = current.joinedMembers[key]
            val lastSeen = if (previous == null) observedAtMs else maxOf(previous.lastSeenMs, observedAtMs)
            val person = signal.personKey ?: previous?.personKey ?: key
            val next = current.copy(
                joinedMembers = current.joinedMembers + (key to GroupCallMember(person, lastSeen)),
                lastActivityMs = maxOf(current.lastActivityMs, observedAtMs),
            )
            if (next == current) current else next
        } else {
            current
        }
    }
    is GroupCallSignal.Leave -> {
        val key = signal.memberKey
        if (current != null && current.callIdHex == signal.callIdHex && key != null) {
            current.copy(
                joinedMembers = current.joinedMembers - key,
                lastActivityMs = maxOf(current.lastActivityMs, observedAtMs),
            )
        } else {
            current
        }
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
    ownDeviceKey: String? = null,
): GroupCallRecord? {
    if (record == null || anyCallActive) return null
    if (record.callIdHex == activeGroupCallIdHex) return null
    // Not in this call, so this device's own entry is a dead session.
    return record.takeIf { isGroupCallLive(it, nowMs, ignoreDeviceKey = ownDeviceKey) }
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
 * The FRESH members of the recorded call [callIdHex] other than [ownMemberKey],
 * or `null` when [record] is absent or about another call. What "nobody else
 * joined" reads when this device leaves.
 */
internal fun otherFreshGroupCallMembers(
    record: GroupCallRecord?,
    callIdHex: String,
    ownMemberKey: String?,
    nowMs: Long,
): Set<String>? {
    if (record == null || record.callIdHex != callIdHex) return null
    return freshGroupCallMembers(record, nowMs, ignoreDeviceKey = ownMemberKey)
}

/**
 * After this device's own `GroupCallLeave`, should it also publish `CallEnd`?
 * Only when the recorded call is the one it left and no OTHER member is fresh.
 * A record about a different call (this device's start lost the tie-break)
 * carries no knowledge of who is in the call it left, so it ends nothing.
 */
internal fun shouldEndGroupCallAfterLeave(
    record: GroupCallRecord?,
    leftCallIdHex: String,
    ownMemberKey: String?,
    nowMs: Long,
): Boolean = otherFreshGroupCallMembers(record, leftCallIdHex, ownMemberKey, nowMs)?.isEmpty() == true
