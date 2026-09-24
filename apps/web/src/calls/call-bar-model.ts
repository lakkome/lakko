// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The collapsed call bar — the web client's seam onto the shared rules.
 *
 * `docs/ux/calls.md` section 4.5 puts a live call away into one bar across the
 * top of the app, so the app stays usable while the call runs. Two of the
 * decisions behind it have no platform in them and live in
 * `crates/lakko-ui-rules/src/call_bar.rs`: whether the bar is on screen, and
 * which of four things its one status line says. They are reached here over
 * wasm; iOS and Android reach the same code over UniFFI.
 *
 * ## Why the status line goes through one function
 *
 * The rule answers the line as a PAIR — a kind, and a running time present only
 * for one of the kinds — and the wasm surface has to split that pair across two
 * calls (`callBarStatusKind` and `callBarElapsedMs`) because wasm-bindgen does
 * not hand back an enum with data. Section 4 says the line carries the state or
 * the time and **never two at once**, and a caller holding two independent
 * reads is one careless `if` away from printing both, or from printing a
 * duration for a call that is still ringing.
 *
 * So {@link callBarStatus} takes both reads in one place and returns one
 * string. Nothing else in this client may call `callBarStatusKind` or
 * `callBarElapsedMs`: the pair is re-joined here, once.
 *
 * ## What this module does NOT decide
 *
 * Whether the bar is shown, and what its line says. Both are the shared rule's,
 * and this module passes the client's facts in and the rule's answer back out.
 * The only judgements here are presentational — which words render for each
 * kind, and how a duration is formatted — and the formatting is the call
 * screen's own `formatCallDuration`, so the bar's `12:04` and the screen's are
 * the same glyphs by construction rather than by coincidence.
 */

import type { CallBarPhase } from "../../wasm/lakko_ffi_wasm";
import type { Wasm } from "../wasm";
import { callControls, type CallControlSpec } from "./call-controls";
import {
  formatCallDuration,
  spokenCallDuration,
  type CallPhaseKind,
} from "./call-screen-model";

/**
 * The shared call-bar rules, as this module needs to see them.
 *
 * Structural rather than `Pick<Wasm, …>` so a test can hand in a spy, following
 * `capture-rule.ts`. The enum comes in as part of the shape rather than being
 * hardcoded as numbers here: its discriminants are a wasm ABI detail, and a
 * client that wrote `2` for `Preparing` would keep compiling after a
 * reordering and start answering about a different phase.
 */
export interface CallBarRules {
  readonly CallBarPhase: {
    readonly Idle: CallBarPhase;
    readonly Ringing: CallBarPhase;
    readonly Preparing: CallBarPhase;
    readonly Calling: CallBarPhase;
    readonly Connecting: CallBarPhase;
    readonly Connected: CallBarPhase;
    readonly Reconnecting: CallBarPhase;
    readonly Ended: CallBarPhase;
  };
  readonly callBarVisible: (phase: CallBarPhase, callScreenUp: boolean) => boolean;
  readonly callBarStatusKind: (phase: CallBarPhase) => string;
  readonly callBarElapsedMs: (
    phase: CallBarPhase,
    connectedForMs: bigint,
  ) => bigint | undefined;
}

/**
 * The claim above, checked by the compiler rather than asserted in prose: a spy
 * is only honest while the real wasm namespace still satisfies this shape, so a
 * signature change in the core fails the build here instead of in a test that
 * was fed a fake all along.
 */
const _wasmSatisfiesCallBarRules: (
  w: Pick<
    Wasm,
    "CallBarPhase" | "callBarVisible" | "callBarStatusKind" | "callBarElapsedMs"
  >,
) => CallBarRules = (w) => w;
void _wasmSatisfiesCallBarRules;

/**
 * Where the call is, as the shared rule needs to see it.
 *
 * This client's store phase (`state/call-state.ts` `CallPhase`) and the rule's
 * enum are not the same set, and the two differences are both deliberate:
 *
 *   - the store has no `Preparing`. An outgoing call is dialled in the same
 *     breath as it is built, so `calling` covers both — which is exactly what
 *     the rule's own note on `Preparing` says, and why both answer `Calling`.
 *   - the store has no `Ringing`: `CallHostContext.syncCallUiFromState` resets
 *     the store for a ringing 1x1 call, because a ringing call takes the whole
 *     screen (§2.2) rather than driving the in-call surface. So the ring is
 *     passed in separately, from the host's own `state()`.
 *
 * The ring is threaded through even though `Idle` and `Ringing` both hide the
 * bar, so this reports what is true rather than something that happens to give
 * the same answer. Deciding for itself which phases are worth asking about is
 * the mistake the rule's own doc names.
 */
export function callBarPhase(
  rules: CallBarRules,
  storePhase: CallPhaseKind,
  ringing: boolean,
): CallBarPhase {
  const p = rules.CallBarPhase;
  if (ringing) return p.Ringing;
  switch (storePhase) {
    case "idle":
      return p.Idle;
    case "calling":
      return p.Calling;
    case "connecting":
      return p.Connecting;
    case "connected":
      return p.Connected;
    case "reconnecting":
      return p.Reconnecting;
    case "ended":
      return p.Ended;
  }
}

/**
 * Whether this call has a bar to be put away INTO.
 *
 * The shared visibility rule, asked with the call screen away: if it would show
 * nothing then there is nothing to collapse to, and collapsing would leave the
 * person looking at the app with a live call and no surface for it anywhere.
 *
 * That is exactly the question "may this call be collapsed", so the chevron on
 * the call screen and its `Esc` shortcut read this instead of carrying their own
 * list of phases — which would be a second copy of section 4.5's three
 * sentences about when the bar is hidden, and would drift from the first. It is
 * what keeps `Esc` from putting an ENDED call away: its overlay still owns the
 * screen until it is dismissed, and the bar does not take ended calls.
 */
export function callBarWouldShow(
  rules: CallBarRules,
  storePhase: CallPhaseKind,
  ringing: boolean,
): boolean {
  return rules.callBarVisible(callBarPhase(rules, storePhase, ringing), false);
}

/** The facts the bar's line is built from. */
export interface CallBarStatusInput {
  readonly phase: CallBarPhase;
  /** When media started flowing, or `null` before it has. */
  readonly connectedAtMs: number | null;
  readonly nowMs: number;
}

/** One line, and what a screen reader reads instead of it. */
export interface CallBarStatusLine {
  /** What the bar prints. */
  readonly text: string;
  /**
   * What a screen reader says (§8). `12:04` read back is a time of day, so a
   * running time is announced as a duration; every other kind already reads as
   * words and is carried through unchanged.
   */
  readonly spoken: string;
}

/**
 * The bar's one status line.
 *
 * The rule decides which of four things it says; this turns that into words,
 * and the running time through the call screen's own formatter so the two
 * surfaces print the same digits.
 *
 * An `elapsed` verdict whose companion value is missing cannot happen — the
 * pair comes from one rule over one phase — and if it ever did, this prints
 * nothing rather than a `0:00` that would state the call had just connected.
 * The unknown-kind arm is the same posture: the bar says nothing rather than
 * inventing a state.
 */
export function callBarStatus(
  rules: CallBarRules,
  input: CallBarStatusInput,
): CallBarStatusLine {
  const kind = rules.callBarStatusKind(input.phase);
  switch (kind) {
    case "calling":
      return { text: "Calling...", spoken: "Calling" };
    case "connecting":
      return { text: "Connecting...", spoken: "Connecting" };
    case "reconnecting":
      return { text: "Reconnecting...", spoken: "Reconnecting" };
    case "elapsed": {
      const since = input.connectedAtMs;
      const ms = rules.callBarElapsedMs(
        input.phase,
        BigInt(since === null ? 0 : Math.max(0, input.nowMs - since)),
      );
      if (ms === undefined) return { text: "", spoken: "" };
      const n = Number(ms);
      return { text: formatCallDuration(n), spoken: spokenCallDuration(n) };
    }
    default:
      return { text: "", spoken: "" };
  }
}

/**
 * The bar's mute button, drawn exactly as the call screen draws it (§4.5).
 *
 * `callControls` is asked for the whole row and the microphone entry taken out
 * of it, rather than the icon and tone being decided a second time here. That
 * is the point: the icon, the tone (`outline` when live, `solid` when muted —
 * §4.1.1's loud state), the accessible name and the disabled state all come
 * from the one definition, so a change to how the screen paints mute moves the
 * bar with it instead of leaving the two to drift.
 *
 * The row's own `disabled` rule comes along with it, and is right here: while a
 * call is only `Calling` there is nothing yet to mute, so the bar shows the
 * control inert rather than absent (§4.1.1 — never a hidden button).
 *
 * `surface` is fixed at `voice` because the microphone entry does not depend on
 * it; if it ever does, this returns `null` rather than a guess.
 */
export function callBarMuteControl(
  storePhase: CallPhaseKind,
  micOn: boolean,
): CallControlSpec | null {
  const row = callControls({
    surface: "voice",
    phase: storePhase,
    micOn,
    cameraOn: false,
    cameraBusy: false,
    canSelectAudioOutput: false,
    nextAudioOutputLabel: null,
  });
  return row.find((spec) => spec.id === "mic") ?? null;
}
