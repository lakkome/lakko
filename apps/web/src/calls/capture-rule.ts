// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Whether a call is holding the microphone and camera — the web client's seam
 * onto the shared rule.
 *
 * `docs/ux/media-messages.md` blocks recording a voice message or a video note
 * while a call owns capture, and this client asked a DIFFERENT question to
 * decide it: "is there a call in this chat's call lane". The two differ for
 * exactly one state — an incoming call that is ringing and has not been
 * answered — where nothing is capturing at all. `call-session.ts` says so
 * itself where it parks a fresh inbound offer: nothing is captured and no peer
 * connection exists until the user accepts.
 *
 * So with a call ringing quietly in a muted conversation (`docs/ux/calls.md`
 * section 2.1.1) the composer refused the one reply that fits the moment — a
 * voice message saying they cannot talk. It was invisible until those quiet
 * surfaces landed: before them a ringing call took the screen, so a recorder
 * disabled behind it could not be noticed.
 *
 * The rule itself lives in `crates/lakko-ui-rules/src/capture.rs` and is
 * reached over wasm; iOS and Android reach the same code over UniFFI. Nothing
 * here decides anything — it converts the two facts this client already holds
 * into what the rule takes, and hands back the rule's answer.
 */

import type { Wasm } from "../wasm";

/**
 * The shared capture rule, as this module needs to see it.
 *
 * Structural rather than `Pick<Wasm, …>` so a test can hand in a spy: vitest
 * runs in bare node with no wasm here. The real wasm namespace is assignable
 * to this, which is what keeps a spy honest.
 */
export interface CaptureRules {
  readonly callHoldsCapture: (
    callInThisChat: boolean,
    unansweredIncomingRing: boolean,
  ) => boolean;
}

/**
 * The claim above, checked by the compiler rather than asserted in prose: a
 * spy is only honest while the real namespace still satisfies this shape, so a
 * signature change in the core fails the build here instead of in a test that
 * was fed a fake all along.
 */
const _wasmSatisfiesCaptureRules: (w: Pick<Wasm, "callHoldsCapture">) => CaptureRules = (w) => w;
void _wasmSatisfiesCaptureRules;

/**
 * The call host, as deciding this needs to see it.
 *
 * Two reads the host already publishes, and no more: a chat screen is JSX and
 * vitest here runs bare node with no Solid transform, so facts derived inside a
 * view are facts nothing checks — which is how a call lane came to answer a
 * question about the microphone in the first place.
 */
export interface CaptureCallLane {
  /** The group whose journal carries the call in progress, or `null`. */
  callLaneGroupIdB64: () => string | null;
  /** The call id of the 1x1 call RINGING on this device, or `null`. */
  ringingCallIdHex: () => string | null;
}

/** The two facts this client holds, as the rule takes them. */
export interface CaptureFacts {
  /** This chat's own call-lane read: a call of any kind occupies it. */
  readonly callInThisChat: boolean;
  /**
   * That call is an incoming offer still being rung, which the person has not
   * accepted — the one state that owns no capture.
   *
   * An OUTGOING call is deliberately not reported here even while it is only
   * dialing: the caller's microphone opens as the call is placed. The
   * asymmetry is the rule's, and widening this to "the call is ringing" would
   * undo it.
   */
  readonly unansweredIncomingRing: boolean;
}

/**
 * Whether the composer's voice and video-note recorder must stand aside.
 *
 * `rules` is `null` until wasm has loaded, and that answers `callInThisChat` —
 * byte-for-byte what this client did before the rule existed, and the
 * conservative side: a recorder that refuses is a message not sent, where one
 * that records over a live call is a take with a call inside it. The window is
 * theoretical in the running app (nothing reaches a chat before wasm), but a
 * fallback that guessed the other way would be a silent regression on a slow
 * load.
 */
export function callHoldsCapture(
  rules: CaptureRules | null,
  facts: CaptureFacts,
): boolean {
  if (rules === null) return facts.callInThisChat;
  return rules.callHoldsCapture(facts.callInThisChat, facts.unansweredIncomingRing);
}

/**
 * {@link callHoldsCapture} for the chat `groupIdB64`, reading the two facts off
 * the call host.
 *
 * The ring is read from the host's own `ringingCallIdHex`, which is non-null
 * exactly while a 1x1 call is in `ringing` — a state `call-session.ts` reaches
 * only for a fresh INBOUND offer, never for an outgoing dial, which is what
 * makes it the rule's `unansweredIncomingRing` rather than merely "is ringing".
 * It is not scoped to this chat and does not need to be: the app holds one call
 * at a time, so a ring anywhere else leaves this chat's lane empty and the rule
 * answers from the first fact alone.
 */
export function callOwnsCaptureIn(
  rules: CaptureRules | null,
  host: CaptureCallLane,
  groupIdB64: string,
): boolean {
  return callHoldsCapture(rules, {
    callInThisChat: host.callLaneGroupIdB64() === groupIdB64,
    unansweredIncomingRing: host.ringingCallIdHex() !== null,
  });
}
