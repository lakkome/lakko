// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which outbound call-signaling frames are worth a push wake.
 *
 * ## What this closes
 *
 * A 1x1 call offer is published as an ordinary MLS application message
 * through {@link ChatClient.broadcastControl}, which fires NO push wake — the
 * deliberate ADR-0042 rule that a control envelope (a routing-secret
 * handshake, a liveness beacon) must not wake devices. A call offer was swept
 * up by that rule, so a callee whose tab was closed or backgrounded learned
 * about the call only on its next foreground poll, i.e. never: the phone did
 * not ring. `docs/ux/calls.md` §2.1 states the requirement plainly — "a call
 * that only rings inside an open chat is not a feature anyone can rely on" —
 * and §7 names the missing piece as a call-kind wake that survives the
 * background filters.
 *
 * ## The rule, and why it is this narrow
 *
 * Only two of the seven signaling bodies may wake anyone:
 *
 *   - **offer** — the ring itself. Nothing else can start one.
 *   - **end** — stops a ring the callee may already be showing, on every
 *     device of theirs, including the ones whose tab is shut. Without it a
 *     cancelled call keeps ringing a closed tab's notification until the user
 *     dismisses it by hand.
 *
 * And not every offer or end does. A call wake reaches an iPhone as a VoIP
 * push, and iOS makes the app report every VoIP push to CallKit as an
 * incoming call; the DS also wakes the sender's own devices. So a wake on a
 * frame that rings nobody is not merely wasted: it rings. Two such frames
 * share the tags above and cannot be told apart by them:
 *
 *   - a mid-call **renegotiation offer** (camera on, screen share) is an offer
 *     for the same call id. Waking for it showed the peer's iPhone an
 *     incoming audio call while the two were already talking;
 *   - the **end of a connected call** is an end. The peer is in the call and
 *     awake; waking for it rang the iPhone again after the call was over.
 *
 * Only the call session knows which offer is the dial and whether the call
 * connected, so the session states the intent with each frame
 * ({@link CallSignalingWake}) and the publisher wakes only when the session
 * asked AND the bytes are an offer or an end. The tag check stays as a floor:
 * a session that asks for a wake on an answer or a candidate still gets none.
 *
 * Everything else is deliberately silent:
 *
 *   - **answer** / **candidate** belong to a call whose peer is, by
 *     construction, already awake and in the call — a wake buys nothing and
 *     costs a push per ICE candidate, which is a burst of them;
 *   - **groupStart** / **groupJoin** / **groupLeave** are group-call
 *     announcements, not a 1x1 ring. `docs/ux/calls.md` §9 puts the group
 *     call redesign out of scope, and waking for a join / leave would push
 *     once per participant churn. A group call's **end** is silent too
 *     (`makeGroupCallControlPublisher`): a group call never rings a closed
 *     device, so its end has no ring to stop.
 *
 * ## Why a byte peek rather than a parse
 *
 * The publisher sees the frame as already-composed prost bytes, and the
 * leading field tag of `CallSignalingPayload.body` names the variant with no
 * decode at all. Two other places in this codebase read the same table —
 * `chat/client.ts` (the foreground receive classifier) and
 * `public/sw-push-decrypt.js` (the background mirror) — so the table lives
 * here once and `chat/client.ts` reads it from here; the service worker
 * cannot import from `src/` and keeps its documented copy.
 *
 * A misread is bounded in both directions: a frame wrongly classified as
 * wakeable costs one extra push, and one wrongly classified as silent costs
 * the wake that the recipient's next poll would have made up anyway. Neither
 * can send a frame, drop a frame, or change what is published.
 */

/**
 * What a call session asks of the publisher for one outbound frame.
 *
 * `"call"`: this frame rings someone or stops a ring (the dial offer, the end
 * of a call that never connected on this device). `"none"`: publish it
 * silently. The session decides; {@link shouldWakeForCallSignaling} still
 * refuses a wake for any frame that is not an offer or an end.
 */
export type CallSignalingWake = "call" | "none";

/** The `CallSignalingPayload.body` variants, as the wire tags name them. */
export type CallSignalingBodyKind =
  | "offer"
  | "answer"
  | "candidate"
  | "end"
  | "groupStart"
  | "groupJoin"
  | "groupLeave"
  | "screenShare";

/**
 * Leading wire tag of an embedded `CallSignalingPayload.body` oneof field
 * (wire type 2, so the tag byte is `(field << 3) | 2`):
 *
 *   field 10 (offer)       -> 0x52
 *   field 11 (answer)      -> 0x5A
 *   field 12 (candidate)   -> 0x62
 *   field 13 (end)         -> 0x6A
 *   field 14 (group_start) -> 0x72
 *   field 15 (group_join)  -> 0x7A
 *   field 16 (group_leave) -> 0x82 0x01 (tag 130, a two-byte varint)
 *   field 17 (screen_share) -> 0x8A 0x01 (tag 138; ADR-0083, a one-to-one
 *                              call's share state; missing here until
 *                              2026-10-29, so it rendered as a garbage bubble)
 *
 * A chat envelope is JSON and starts with `{` (0x7B); every `lakko/...`
 * service envelope starts with `l` (0x6C). Neither overlaps this set.
 */
const SINGLE_BYTE_TAGS: ReadonlyMap<number, CallSignalingBodyKind> = new Map([
  [0x52, "offer"],
  [0x5a, "answer"],
  [0x62, "candidate"],
  [0x6a, "end"],
  [0x72, "groupStart"],
  [0x7a, "groupJoin"],
]);

/**
 * Name the signaling body these bytes carry, or `null` when they are not
 * call signaling at all.
 *
 * Reads the leading tag only — it never decodes the body, so it cannot
 * throw and costs the same on every frame.
 */
export function callSignalingBodyKind(
  bytes: Uint8Array,
): CallSignalingBodyKind | null {
  if (bytes.length === 0) return null;
  const first = bytes[0] as number;
  const single = SINGLE_BYTE_TAGS.get(first);
  if (single !== undefined) return single;
  if (first === 0x82 && bytes.length > 1 && bytes[1] === 0x01) {
    return "groupLeave";
  }
  if (first === 0x8a && bytes.length > 1 && bytes[1] === 0x01) {
    return "screenShare";
  }
  return null;
}

/**
 * Are these bytes call signaling of any kind?
 *
 * The receive-side classifier both `chat/client.ts` and the service-worker
 * mirror ask, kept here so the tag table has one definition.
 */
export function looksLikeCallSignaling(bytes: Uint8Array): boolean {
  return callSignalingBodyKind(bytes) !== null;
}

/**
 * Should publishing this signaling frame also fan a push wake out to the
 * group's subscribed devices?
 *
 * `true` only when the sender asked for a call wake AND the frame is an offer
 * or an end; `false` for everything else — including anything that is not
 * call signaling, so a caller that hands over the wrong bytes fails toward
 * silence rather than toward a push.
 */
export function shouldWakeForCallSignaling(
  bytes: Uint8Array,
  asked: CallSignalingWake,
): boolean {
  if (asked !== "call") return false;
  const kind = callSignalingBodyKind(bytes);
  return kind === "offer" || kind === "end";
}
