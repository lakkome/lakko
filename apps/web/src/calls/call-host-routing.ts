// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Where an inbound call-signaling row goes once the 1x1 call lives at the app
 * level rather than inside one chat view.
 *
 * ## Why the decision moved
 *
 * The 1x1 `CallSession` used to be owned by `routes/Chat.tsx`, and signaling
 * was only ever read by that chat's own receive loop. An offer to a chat the
 * user did not have open sat on the DS until they opened it; an offer that
 * arrived while they were on Home, Contacts or another chat never rang; and
 * leaving the chat hung the call up. The call now lives in
 * `CallHostContext.tsx`, mounted for the whole signed-in session, and is fed
 * from every receive path — the open chat's long poll and the background
 * receive for every other joined group — through one exactly-once queue keyed
 * on `(group, DS message id)`.
 *
 * Folding many groups into one call slot adds three questions a per-chat view
 * never had to ask, and they are answered here, in a pure function, because
 * vitest runs in plain node and nothing inside a `.tsx` provider can be tested:
 *
 *   - an `answer` / `iceCandidate` / `end` must name the active call AND come
 *     from the group that call lives in — the same 16-byte call id arriving on
 *     a different group's journal is not this call (C-12, widened);
 *   - a fresh `offer` for a different call while one is ringing or live is
 *     answered `End{busy}` rather than silently dropped, so the caller stops
 *     ringing ("one call at a time");
 *   - a group call or a call-link call starting or in progress (both SFU
 *     sessions, in the call host's one call slot — `call-slot.ts`) also makes
 *     this device busy for a 1x1 offer.
 *
 * Unchanged from the chat view, and pinned in the tests: C-11 (a group-control
 * body never reaches the 1x1 session) and the rule that a call that ended here
 * never rings again.
 *
 * ## The two rules this module no longer decides for itself
 *
 * Whether an offer is too old to ring for, and whether the conversation is
 * muted, are one question with one answer, and that answer lives in
 * `crates/lakko-ui-rules/src/ring.rs` — reached here as a {@link RingVerdict}
 * the caller computed through `ring-rule.ts`. It used to be a local
 * `isStaleOffer` against a 60 s constant that disagreed with the 45 s ring
 * window `call-session.ts` armed its timers from, so an offer between the two
 * rang on a call the caller had already given up on. Mute was not consulted
 * at all.
 *
 * The verdict is an INPUT rather than something computed here because this
 * module is pure so that vitest (bare node, no wasm) can run it; the caller
 * already holds the wasm namespace to parse the frame at all.
 */
import { type SignalingRoute } from "./call-signaling-dispatch";
import type { RingVerdict } from "./ring-rule";

/** The 1x1 call currently occupying the slot, as routing needs to see it. */
export interface ActiveOneToOneCall {
  readonly callIdHex: string;
  readonly groupIdB64: string;
}

export interface InboundSignalingInput {
  /** `routeSignalingKind(parsed.kind)`. */
  readonly route: SignalingRoute;
  /** `parsed.kind`, needed to tell an offer from an answer / ICE candidate. */
  readonly kind: string;
  readonly callIdHex: string;
  /** The group whose journal carried the row. */
  readonly groupIdB64: string;
  /**
   * What the shared rule says an OFFER does here, from the DS row's own
   * timestamp and the conversation's mute flag (`ring-rule.ts`). Read only
   * for a fresh offer; an answer, a candidate or an end names a call this
   * device is already in, and neither staleness nor mute has anything to say
   * about those.
   */
  readonly ring: RingVerdict;
  /** The 1x1 call in the slot, or `null` when the slot is free (idle / done). */
  readonly active: ActiveOneToOneCall | null;
  /** Call ids of 1x1 calls that ended on this device this session. */
  readonly endedCallIds: ReadonlySet<string>;
  /** A group or call-link (SFU) call is starting, waiting for admission, or
   *  live on this device. */
  readonly groupCallBusy: boolean;
  /** The group-call record for this row's group already knows this call id. */
  readonly groupRecordKnowsCall: boolean;
}

/** What happens to the group-call record. */
export type GroupRecordAction = "none" | "apply-control" | "end";

/** What happens on the 1x1 side. */
export type OneToOneAction =
  /** Nothing — not addressed to the call in the slot, or stale / ended. */
  | "drop"
  /** Open a ringing answerer session for this offer. */
  | "ring"
  /** Publish `End{busy}` to the offering group; do not ring. */
  | "busy"
  /**
   * Take the call, withhold the interruption: the conversation is muted.
   *
   * Everything `ring` does EXCEPT the ringtone, the vibration and the screen
   * that takes over the tab. The call still occupies the slot and still
   * writes its row to the call log when it ends — a person who muted a
   * conversation asked not to be interrupted by it, not to be kept from
   * finding out who tried to reach them.
   *
   * **And it stays answerable.** This doc used to concede the opposite: that
   * because the full-screen incoming call was the only Accept the web client
   * had, and it is the surface being withheld, a muted call could not be
   * answered at all. `docs/ux/calls.md` section 2.1.1 rejects that reading —
   * it made muting a conversation into half-blocking the person — and names
   * two quiet surfaces the call is offered on for the whole ring window: a
   * panel inside that conversation, and Accept / Decline on that
   * conversation's row in the list. Neither of them is a surface this module
   * knows about; what it decides is that the call is TAKEN, which is what
   * gives them something to offer (`calls/quiet-call-offer.ts`).
   */
  | "silent"
  /** Hand the body to the session in the slot. */
  | "apply";

export interface InboundSignalingDecision {
  readonly groupRecord: GroupRecordAction;
  readonly oneToOne: OneToOneAction;
}

const DROP: InboundSignalingDecision = { groupRecord: "none", oneToOne: "drop" };

/** Decide one inbound row. Pure; see the module header for the rules. */
export function decideInboundSignaling(input: InboundSignalingInput): InboundSignalingDecision {
  switch (input.route) {
    case "drop":
      return DROP;
    case "group":
      // C-11: group-control bodies feed the group-call record and nothing else.
      return { groupRecord: "apply-control", oneToOne: "drop" };
    case "end": {
      const groupRecord: GroupRecordAction = input.groupRecordKnowsCall ? "end" : "none";
      return { groupRecord, oneToOne: isForActiveCall(input) ? "apply" : "drop" };
    }
    case "oneToOne":
      return { groupRecord: "none", oneToOne: decideOneToOne(input) };
  }
}

function isForActiveCall(input: InboundSignalingInput): boolean {
  const a = input.active;
  return a !== null && a.callIdHex === input.callIdHex && a.groupIdB64 === input.groupIdB64;
}

function decideOneToOne(input: InboundSignalingInput): OneToOneAction {
  if (isForActiveCall(input)) {
    // The call in the slot: its answer, its ICE, or a renegotiation offer.
    return "apply";
  }
  // Anything that is not a fresh offer names a call this device is not in.
  if (input.kind !== "offer") return "drop";
  // A call that already ended here never rings again, and is not answered busy
  // either: it is over, whatever the journal still holds.
  if (input.endedCallIds.has(input.callIdHex)) return "drop";
  // The caller gave up long ago, or the age could not be established. Nobody
  // is waiting for a busy either.
  if (input.ring === "tooLate") return "drop";
  // Busy outranks mute: the offering side is waiting on an answer, and `End
  // {busy}` is the answer whether or not this device would have rung.
  if (input.active !== null || input.groupCallBusy) return "busy";
  return input.ring === "silent" ? "silent" : "ring";
}
