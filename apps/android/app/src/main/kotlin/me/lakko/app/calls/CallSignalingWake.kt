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
 * Exactly two of the seven signaling bodies wake anyone:
 *
 *   * **offer** — the ring itself. Nothing else can start one.
 *   * **end** — stops a ring the callee may already be showing, on every device
 *     of theirs, including the ones whose app is shut. Without it a cancelled
 *     call keeps ringing until the user dismisses it by hand.
 *
 * Everything else is deliberately silent:
 *
 *   * **answer** / **candidate** belong to a call whose peer is, by
 *     construction, already awake and in the call — a wake buys nothing and
 *     costs a push per ICE candidate, which is a burst of them;
 *   * **groupStart** / **groupJoin** / **groupLeave** are group-call
 *     announcements, not a 1x1 ring. `docs/ux/calls.md` §9 puts the group call
 *     redesign out of scope, and waking for a join / leave would push once per
 *     participant churn.
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

/** The `CallSignalingPayload.body` variants, as `schemas/mls-app/call_signaling_v1.proto` names them. */
internal enum class CallSignalingBodyKind {
    OFFER,
    ANSWER,
    CANDIDATE,
    END,
    GROUP_START,
    GROUP_JOIN,
    GROUP_LEAVE,
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
    return null
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
 * `true` for an offer and an end, `false` for everything else — including
 * anything that is not call signaling at all, so a caller that hands over the
 * wrong bytes fails toward silence rather than toward a push.
 */
internal fun shouldWakeForCallSignaling(bytes: ByteArray): Boolean =
    when (callSignalingBodyKind(bytes)) {
        CallSignalingBodyKind.OFFER, CallSignalingBodyKind.END -> true
        else -> false
    }
