// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * Which outbound call-signaling frames are worth a push wake, and the one
 * definition of the `CallSignalingPayload.body` tag table both the receive-side
 * sniff and the wake decision read.
 *
 * ## What this closes
 *
 * A 1x1 call offer is published as an ordinary MLS application message through
 * `ChatRepository.publishApplicationMessage`, which fired NO push wake — the
 * deliberate ADR-0042 rule that a control envelope (a routing-secret handshake,
 * a liveness beacon) must not wake devices. A call offer was swept up by that
 * rule, so a callee whose app was backgrounded or closed learned about the call
 * only on its next foreground poll, i.e. never: the phone did not ring.
 * `docs/ux/calls.md` §2.1 states the requirement plainly — "a call that only
 * rings inside an open chat is not a feature anyone can rely on" — and §7 names
 * the missing piece as a call-kind wake that survives the background filters.
 *
 * ## The rule, and why it is this narrow
 *
 * Only two of the seven signaling bodies may wake anyone:
 *
 *   * **offer** — the ring itself. Nothing else can start one.
 *   * **end** — stops a ring the callee may already be showing, on every device
 *     of theirs, including the ones whose app is shut. Without it a cancelled
 *     call keeps ringing until the user dismisses it by hand.
 *
 * And not every offer or end does. A call wake reaches an iPhone as a VoIP
 * push, and iOS makes the app report every VoIP push to CallKit as an incoming
 * call; the DS also wakes the sender's own devices. So a wake on a frame that
 * rings nobody is not merely wasted: it rings. Two such frames share the tags
 * above and cannot be told apart by them:
 *
 *   * a mid-call **renegotiation offer** (camera on, screen share) is an offer
 *     for the same call id. Waking for it showed the peer's iPhone an incoming
 *     audio call while the two were already talking;
 *   * the **end of a connected call** is an end. The peer is in the call and
 *     awake; waking for it rang the iPhone again after the call was over.
 *
 * Only the call session knows which offer is the dial and whether the call
 * connected, so it states the intent with each frame ([CallSignalingWake],
 * decided by [OneToOneCallWakes]) and the publisher wakes only when the session
 * asked AND the bytes are an offer or an end. The tag check stays as a floor:
 * a session that asks for a wake on an answer or a candidate still gets none.
 *
 * Everything else is deliberately silent:
 *
 *   * **answer** / **candidate** belong to a call whose peer is, by
 *     construction, already awake and in the call — a wake buys nothing and
 *     costs a push per ICE candidate, which is a burst of them;
 *   * **groupStart** / **groupJoin** / **groupLeave** are group-call
 *     announcements, not a 1x1 ring. `docs/ux/calls.md` §9 puts the group call
 *     redesign out of scope, and waking for a join / leave would push once per
 *     participant churn. A group call's **end** is silent too
 *     ([GROUP_CALL_CONTROL_WAKE]): a group call never rings a closed device, so
 *     its end has no ring to stop.
 *
 * ## Why a byte peek rather than a parse
 *
 * The publisher sees the frame as already-composed prost bytes, and the leading
 * field tag of `CallSignalingPayload.body` names the variant with no decode at
 * all. The alternative — `uniffi.lakko_ffi_uniffi.parseCallSignaling` — would
 * cost an FFI hop per frame and, worse, cannot be exercised from a host unit
 * test at all (`liblakko_ffi_uniffi.so` does not exist in the `test` source
 * set), so the rule that decides whether a phone rings would be untestable.
 *
 * A misread is bounded in both directions: a frame wrongly classified as
 * wakeable costs one extra push, and one wrongly classified as silent costs the
 * wake that the recipient's next poll would have made up anyway. Neither can
 * send a frame, drop a frame, or change what is published.
 *
 * The table lives here once. `me.lakko.app.chat.looksLikeCallSignaling` (the
 * foreground receive classifier, reused by the background notification filter)
 * delegates to [callSignalingBodyKind]; a second copy would drift silently the
 * next time a field number moves.
 */

/**
 * What a call session asks of the publisher for one outbound frame.
 *
 * [CALL]: this frame rings someone or stops a ring (the dial offer, the end of
 * a call that never connected on this device). [NONE]: publish it silently.
 * The session decides; [shouldWakeForCallSignaling] still refuses a wake for
 * any frame that is not an offer or an end.
 */
public enum class CallSignalingWake { CALL, NONE }

/**
 * The wake each outbound frame of one 1x1 call asks for.
 *
 * Holds the one fact the bytes do not carry: whether this call ever reached
 * `Connected` on this device. Kept out of [CallSession] so the rule runs in a
 * host unit test, where libwebrtc and the native core are absent.
 */
internal class OneToOneCallWakes {
    @Volatile
    private var everConnected: Boolean = false

    /** The call reached `Connected` on this device. Never undone. */
    fun markConnected() {
        everConnected = true
    }

    /** The dial: the one offer that rings the callee. */
    fun dialOffer(): CallSignalingWake = CallSignalingWake.CALL

    /** A renegotiation of a call already past its dial (camera on / off): the
     *  peer is in the call. */
    fun renegotiationOffer(): CallSignalingWake = CallSignalingWake.NONE

    /** Answers and ICE candidates: the peer is awake and in the call. */
    fun inCall(): CallSignalingWake = CallSignalingWake.NONE

    /** An End for a call that never connected here stops a ring on the other
     *  side's devices, including ones whose app is closed: it wakes. An End for
     *  a connected call reaches a peer that is in the call: silent. */
    fun end(): CallSignalingWake =
        if (everConnected) CallSignalingWake.NONE else CallSignalingWake.CALL
}

/**
 * The wake every group-call control frame asks for: none, `end` included.
 *
 * A group call never rings a closed device (start / join / leave were always
 * silent), so a group `end` stops no ring anywhere. Its call wake reached
 * iPhones as a VoIP push, which iOS reports to CallKit as an incoming call, and
 * showed a phantom incoming call after every group call ended.
 */
internal val GROUP_CALL_CONTROL_WAKE: CallSignalingWake = CallSignalingWake.NONE

/** The `CallSignalingPayload.body` variants, as `schemas/mls-app/call_signaling_v1.proto` names them. */
internal enum class CallSignalingBodyKind {
    OFFER,
    ANSWER,
    CANDIDATE,
    END,
    GROUP_START,
    GROUP_JOIN,
    GROUP_LEAVE,
    /** ADR-0083: a one-to-one call's screen-share state (field 17). */
    SCREEN_SHARE,
}

/**
 * Leading wire tag of an embedded `CallSignalingPayload.body` oneof field
 * (wire type 2, so the tag varint is `(field shl 3) or 2`):
 *
 *   field 10 (offer)       -> 0x52
 *   field 11 (answer)      -> 0x5A
 *   field 12 (candidate)   -> 0x62
 *   field 13 (end)         -> 0x6A
 *   field 14 (group_start) -> 0x72
 *   field 15 (group_join)  -> 0x7A
 *   field 16 (group_leave) -> 0x82 0x01 (tag 130, a TWO-byte varint — a field
 *                            number above 15 no longer fits one byte, which is
 *                            exactly how a first-byte sniff silently misses it)
 *   field 17 (screen_share) -> 0x8A 0x01 (tag 138, ADR-0083; missing here until
 *                            2026-10-29, so a one-to-one call's share state
 *                            rendered as a garbage chat bubble)
 *
 * A chat envelope is JSON and starts with `{` (0x7B); every `lakko/...` service
 * envelope starts with `l` (0x6C). Neither overlaps this set.
 */
private val SINGLE_BYTE_TAGS: Map<Int, CallSignalingBodyKind> = mapOf(
    0x52 to CallSignalingBodyKind.OFFER,
    0x5A to CallSignalingBodyKind.ANSWER,
    0x62 to CallSignalingBodyKind.CANDIDATE,
    0x6A to CallSignalingBodyKind.END,
    0x72 to CallSignalingBodyKind.GROUP_START,
    0x7A to CallSignalingBodyKind.GROUP_JOIN,
)

/**
 * Name the signaling body these bytes carry, or `null` when they are not call
 * signaling at all.
 *
 * Reads the leading tag only — it never decodes the body, so it cannot throw
 * and costs the same on every frame.
 *
 * The group tags additionally require the body's first field to be the 16-byte
 * `call_id` (`len`, then 0x0A 0x10). 0x72 and 0x7A are the ASCII letters `r`
 * and `z`, so a legacy plain-UTF-8 message opening with either letter would
 * otherwise be swallowed as call signaling; 0x10 is a control byte no typed
 * text carries at that offset. Every group body is composed by the one Rust
 * encoder (prost writes fields in number order, and `call_id` is mandatory and
 * exactly 16 bytes), and its whole body is far below 128 bytes, so the length
 * is always one varint byte. The 1x1 tags keep their first-byte-only test,
 * which is what shipped and what the foreground timeline has always used.
 */
internal fun callSignalingBodyKind(bytes: ByteArray): CallSignalingBodyKind? {
    if (bytes.isEmpty()) return null
    val first = bytes[0].toInt() and 0xFF
    val single = SINGLE_BYTE_TAGS[first]
    if (single != null) {
        val lengthAt = when (single) {
            CallSignalingBodyKind.GROUP_START, CallSignalingBodyKind.GROUP_JOIN -> 1
            else -> return single
        }
        return if (opensWithCallId(bytes, lengthAt)) single else null
    }
    if (first == 0x82 && bytes.size > 1 && (bytes[1].toInt() and 0xFF) == 0x01) {
        return if (opensWithCallId(bytes, 2)) CallSignalingBodyKind.GROUP_LEAVE else null
    }
    if (first == 0x8A && bytes.size > 1 && (bytes[1].toInt() and 0xFF) == 0x01) {
        // The share state carries a signature, so this body can pass 127 bytes
        // and its length is then a two-byte varint.
        return if (opensWithCallIdAfterVarint(bytes, 2)) CallSignalingBodyKind.SCREEN_SHARE else null
    }
    return null
}

/** [opensWithCallId] for a body whose length varint may take one or two
 *  bytes. */
private fun opensWithCallIdAfterVarint(bytes: ByteArray, lengthAt: Int): Boolean {
    if (bytes.size <= lengthAt) return false
    val firstLengthByte = bytes[lengthAt].toInt() and 0xFF
    val headerAt = if (firstLengthByte < 0x80) {
        lengthAt + 1
    } else {
        if (bytes.size <= lengthAt + 1 || (bytes[lengthAt + 1].toInt() and 0xFF) >= 0x80) return false
        lengthAt + 2
    }
    if (bytes.size < headerAt + 2) return false
    return (bytes[headerAt].toInt() and 0xFF) == 0x0A &&
        (bytes[headerAt + 1].toInt() and 0xFF) == 0x10
}

/** `bytes[lengthAt]` is a one-byte body length, followed by the `call_id`
 *  field header: tag 0x0A (field 1, length-delimited), length 0x10 (16). */
private fun opensWithCallId(bytes: ByteArray, lengthAt: Int): Boolean {
    if (bytes.size < lengthAt + 3) return false
    val bodyLength = bytes[lengthAt].toInt() and 0xFF
    if (bodyLength >= 0x80) return false
    return (bytes[lengthAt + 1].toInt() and 0xFF) == 0x0A &&
        (bytes[lengthAt + 2].toInt() and 0xFF) == 0x10
}

/**
 * Should publishing this signaling frame also fan a push wake out to the
 * group's subscribed devices?
 *
 * `true` only when the sender asked for a call wake AND the frame is an offer
 * or an end; `false` for everything else — including anything that is not call
 * signaling at all, so a caller that hands over the wrong bytes fails toward
 * silence rather than toward a push.
 */
internal fun shouldWakeForCallSignaling(bytes: ByteArray, asked: CallSignalingWake): Boolean {
    if (asked != CallSignalingWake.CALL) return false
    return when (callSignalingBodyKind(bytes)) {
        CallSignalingBodyKind.OFFER, CallSignalingBodyKind.END -> true
        else -> false
    }
}
