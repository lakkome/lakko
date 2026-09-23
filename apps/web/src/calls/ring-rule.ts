// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Whether an arriving call offer rings on this device — the web client's seam
 * onto the shared rule.
 *
 * `docs/ux/calls.md` section 2.1 states two rules, and until this existed the
 * web client answered one of them by hand and the other not at all:
 *
 *   - **"a muted chat never rings"** was implemented nowhere. Mute is stored,
 *     synced across a person's devices and drawn on every conversation row,
 *     and no ring path read it.
 *   - **the 45 s window** was a literal in `call-session.ts`, beside a
 *     SECOND, DIFFERENT literal in `call-signaling-dispatch.ts` (a 60 s
 *     staleness gate) that decided the same question from the other end. So
 *     an offer 50 s old rang here while the caller had already given up and
 *     filed a missed call — the exact drift the shared rule exists to stop.
 *
 * The rule itself lives in `crates/lakko-ui-rules/src/ring.rs` and is reached
 * over wasm; iOS and Android reach the same code over UniFFI. Nothing in this
 * module decides anything — it converts what the web client holds (a DS row
 * timestamp, a mute flag) into what the rule takes, and the rule's answer
 * into the string union the rest of the client switches on.
 *
 * ## Why the verdict is computed here and not inside the routing
 *
 * `call-host-routing.ts` is a pure module precisely so vitest (bare node, no
 * wasm, no JSX transform) can run it. Reaching wasm from there would make it
 * untestable, so the caller — which already has the wasm namespace in hand to
 * parse the frame at all — asks the rule and hands the routing the answer.
 */

import type { Wasm } from "../wasm";

/**
 * The shared ring rule, as this module needs to see it.
 *
 * Structural and widened to `number` rather than `Pick<Wasm, …>` so a test can
 * hand in a plain object: the generated `RingDecision` is a TypeScript `enum`,
 * and its members are literal enum types that no object literal satisfies. The
 * real wasm namespace is assignable to this, which is what keeps the fake
 * honest.
 */
export interface RingRules {
  readonly ringDecision: (muted: boolean, offerAgeMs: bigint) => number;
  readonly RingDecision: {
    readonly Ring: number;
    readonly Silent: number;
    readonly TooLate: number;
  };
}

/** The real rule, for a caller holding the wasm namespace. */
export function ringRulesOf(wasm: Pick<Wasm, "ringDecision" | "RingDecision">): RingRules {
  return wasm;
}

/**
 * What an arriving offer does to this device.
 *
 * The three answers of `lakko_ui_rules::ring::RingDecision`, as strings. The
 * distinction between `silent` and `tooLate` is load-bearing and must not be
 * collapsed: both withhold the ring, but `silent` is a call that is happening
 * right now and can still be answered, and `tooLate` is one that is over.
 */
export type RingVerdict = "ring" | "silent" | "tooLate";

/**
 * How old the offer is, from the DS row's own server timestamp.
 *
 * Never a local clock and never anything the caller stamped: the two devices
 * in a call share no clock, and the window only works if both read it the
 * same way. The DS timestamp is the one reading both of them see.
 *
 * `null` when the timestamp does not parse — the caller treats that as an
 * offer whose age cannot be established, which does not ring.
 *
 * A row stamped in the FUTURE (a server clock slightly ahead of this device)
 * clamps to zero rather than going negative: an age is a duration, the rule
 * takes an unsigned one, and a fresh-looking row is the honest reading of a
 * stamp that has not happened yet.
 */
export function offerAgeMs(rowCreatedAt: string, nowMs: number): number | null {
  const createdMs = Date.parse(rowCreatedAt);
  if (Number.isNaN(createdMs)) return null;
  return Math.max(0, nowMs - createdMs);
}

/**
 * Ask the shared rule what an arriving offer does here.
 *
 * An unreadable DS timestamp is `tooLate`, which is what this client already
 * did before the rule existed and is the fail-closed direction: a call whose
 * age cannot be established does not ring. Note that it is NOT reported as
 * `silent` — `silent` promises a live call, and this is a call nothing can be
 * said about.
 */
export function ringVerdictFor(
  rules: RingRules,
  input: {
    readonly muted: boolean;
    readonly rowCreatedAt: string;
    readonly nowMs: number;
  },
): RingVerdict {
  const age = offerAgeMs(input.rowCreatedAt, input.nowMs);
  if (age === null) return "tooLate";
  return ringVerdictAt(rules, input.muted, age);
}

/** {@link ringVerdictFor} for a caller that already has the age. */
export function ringVerdictAt(rules: RingRules, muted: boolean, ageMs: number): RingVerdict {
  const decision = rules.ringDecision(muted, BigInt(Math.max(0, Math.trunc(ageMs))));
  if (decision === rules.RingDecision.TooLate) return "tooLate";
  if (decision === rules.RingDecision.Silent) return "silent";
  return "ring";
}
