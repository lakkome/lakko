// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Who is speaking in a group call (`docs/ux/group-call-grid.md` sections 5.2
 * and 7.3): the web side of the shared rule.
 *
 * The rule itself (what counts as sound for a person, when a ring comes on
 * and goes off, who holds the floor and for how long) is written once, in
 * Rust (`crates/lakko-ui-rules`, `speaking`), and reached here over wasm; iOS
 * and Android reach the same rule over UniFFI, so the three clients light
 * the same rings and move the large tile at the same moments. Nothing of it
 * is restated in this file: no threshold, no timing, no arithmetic on a
 * level.
 *
 * What is here:
 *
 *  - {@link SpeakingRule}: the part of the shared tracker this client calls;
 *  - {@link sharedSpeakingRules}: how one is made (a test hands in its own
 *    to see what the client asked);
 *  - {@link readSpeech}: the tracker's answers in the shape the call store
 *    takes.
 *
 * Reading a level is `audio-level.ts`; the loop that reads everyone is
 * `speaking-monitor.ts`.
 */

import { SpeakingTracker as WasmSpeakingTracker } from "../../wasm/lakko_ffi_wasm";

/**
 * The part of the shared `SpeakingTracker` this client calls. One per call.
 * People are the roster ids; levels are the root mean square of decoded
 * samples, `0..1`; times are one millisecond clock.
 */
export interface SpeakingRule {
  /** Name this device's own person: they have a ring and never the floor. */
  setLocal(person?: string | null): void;
  /** One level reading; no level is "could not be read". */
  sample(person: string, level: number | null | undefined, nowMs: number): void;
  /** The person is muted: no ring and no floor, for as long as this is
   *  called instead of `sample`. */
  silence(person: string): void;
  /** The person left the call. */
  forget(person: string): void;
  /** Forget everyone who is not in the roster as it is now. */
  keepOnly(people: string[]): void;
  /** Settle who holds the floor; `order` is the whole roster, in order. */
  decideFloor(order: string[], nowMs: number): string | undefined;
  /** Who holds the floor, as last decided. */
  floor(): string | undefined;
  /** Everyone whose ring is on, sorted by id. */
  speaking(): string[];
  isSpeaking(person: string): boolean;
  /** Everyone who has held the floor, sorted by id. */
  floorHolders(): string[];
  /** When each of `floorHolders` last held it, in that order. */
  floorHeldAtMs(): Float64Array;
  lastHeldFloorMs(person: string): number | undefined;
  /** What counts as sound for the person now (for the diagnostics line). */
  threshold(person: string): number;
  /** Release the tracker. It holds memory outside the garbage collector's
   *  reach, so whoever made it calls this exactly once. */
  free(): void;
}

/** How a call gets its tracker. */
export interface SpeakingRules {
  /** A tracker of a call nobody has been heard in; throws while the wasm
   *  module is not ready. */
  create(): SpeakingRule;
}

/** The rule every client shares, over wasm. */
export const sharedSpeakingRules: SpeakingRules = {
  create: () => new WasmSpeakingTracker(),
};

/** What the call store is told after a reading of everyone. */
export interface SpeakingSnapshot {
  /** Everyone whose ring is on, this device's own person included. */
  readonly speakingIds: ReadonlySet<string>;
  /** The one person holding the floor, or `null`. Never this device's own. */
  readonly activeSpeakerId: string | null;
  /** When each person last held the floor. */
  readonly lastSpokeAtMs: Readonly<Record<string, number>>;
}

/**
 * The tracker's answers as a snapshot. Read it after `decideFloor`: the
 * floor is whatever was last decided. Copies only; nothing is decided here.
 */
export function readSpeech(rule: SpeakingRule): SpeakingSnapshot {
  const holders = rule.floorHolders();
  const heldAt = rule.floorHeldAtMs();
  const lastSpokeAtMs: Record<string, number> = {};
  holders.forEach((id, i) => {
    const at = heldAt[i];
    if (at !== undefined) lastSpokeAtMs[id] = at;
  });
  return {
    speakingIds: new Set(rule.speaking()),
    activeSpeakerId: rule.floor() ?? null,
    lastSpokeAtMs,
  };
}
