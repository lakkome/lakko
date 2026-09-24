// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The seam onto the shared ring rule.
 *
 * What the rule DECIDES is not asserted here — that is
 * `crates/lakko-ui-rules/src/ring.rs`'s own test suite, and restating it in
 * TypeScript would be a second copy of exactly the thing this arc exists to
 * remove. What is asserted is everything on this side of the boundary: which
 * clock the age comes from, that it is never handed over negative, that an
 * unreadable timestamp never reaches the rule at all, and that each of the
 * three answers arrives intact rather than being collapsed.
 */
import { describe, expect, it } from "vitest";

import {
  offerAgeMs,
  offerExpiresAtMs,
  ringVerdictAt,
  ringVerdictFor,
  type RingRules,
  type RingWindow,
} from "./ring-rule";

/** The rule's three discriminants, as `lakko-ui-rules` numbers them. */
const DECISIONS = { Ring: 0, Silent: 1, TooLate: 2 } as const;

/**
 * A stand-in that RECORDS what it was asked and answers what the test
 * scripted. It does not implement the rule — a fake that did would let this
 * file pass while the real rule said something else.
 */
function fakeRules(answer: number): RingRules & {
  readonly asked: { muted: boolean; ageMs: bigint }[];
} {
  const asked: { muted: boolean; ageMs: bigint }[] = [];
  return {
    asked,
    RingDecision: DECISIONS,
    ringDecision: (muted, ageMs) => {
      asked.push({ muted, ageMs });
      return answer;
    },
  };
}

const NOW = Date.parse("2026-09-20T12:00:00.000Z");

describe("offerAgeMs", () => {
  it("measures from the DS row's own timestamp", () => {
    expect(offerAgeMs(new Date(NOW - 12_345).toISOString(), NOW)).toBe(12_345);
  });

  it("clamps a row stamped in the future to zero rather than going negative", () => {
    // A server clock a little ahead of this device. An age is a duration and
    // the rule takes an unsigned one, so a negative would not survive the
    // crossing at all.
    expect(offerAgeMs(new Date(NOW + 5_000).toISOString(), NOW)).toBe(0);
  });

  it("reports an unreadable timestamp rather than guessing an age", () => {
    expect(offerAgeMs("not a timestamp", NOW)).toBeNull();
    expect(offerAgeMs("", NOW)).toBeNull();
  });
});

describe("ringVerdictFor", () => {
  it("hands the rule the age off the DS row and the conversation's mute flag", () => {
    const rules = fakeRules(DECISIONS.Ring);
    ringVerdictFor(rules, {
      muted: true,
      rowCreatedAt: new Date(NOW - 3_000).toISOString(),
      nowMs: NOW,
    });
    expect(rules.asked).toEqual([{ muted: true, ageMs: 3_000n }]);
  });

  it("carries each of the rule's three answers through intact", () => {
    const fresh = { muted: false, rowCreatedAt: new Date(NOW).toISOString(), nowMs: NOW };
    expect(ringVerdictFor(fakeRules(DECISIONS.Ring), fresh)).toBe("ring");
    expect(ringVerdictFor(fakeRules(DECISIONS.Silent), fresh)).toBe("silent");
    expect(ringVerdictFor(fakeRules(DECISIONS.TooLate), fresh)).toBe("tooLate");
  });

  it("never collapses silent into too-late", () => {
    // Both withhold the ring and they are NOT the same answer: one is a call
    // happening right now that can still be answered, the other is over.
    const fresh = { muted: true, rowCreatedAt: new Date(NOW).toISOString(), nowMs: NOW };
    expect(ringVerdictFor(fakeRules(DECISIONS.Silent), fresh)).not.toBe(
      ringVerdictFor(fakeRules(DECISIONS.TooLate), fresh),
    );
  });

  it("does not ask the rule at all when the row's age cannot be read", () => {
    // Fail-closed, and in the right direction: a call nothing can be said
    // about is reported as over, not as a live call that must not interrupt.
    const rules = fakeRules(DECISIONS.Ring);
    expect(ringVerdictFor(rules, { muted: false, rowCreatedAt: "nope", nowMs: NOW })).toBe(
      "tooLate",
    );
    expect(rules.asked).toEqual([]);
  });
});

describe("ringVerdictAt", () => {
  it("refuses to hand a negative age across the boundary", () => {
    const rules = fakeRules(DECISIONS.Ring);
    ringVerdictAt(rules, false, -1_000);
    expect(rules.asked).toEqual([{ muted: false, ageMs: 0n }]);
  });

  it("truncates a fractional age rather than failing on it", () => {
    const rules = fakeRules(DECISIONS.Ring);
    ringVerdictAt(rules, false, 1_500.75);
    expect(rules.asked).toEqual([{ muted: false, ageMs: 1_500n }]);
  });
});

/**
 * The shared window as a deadline, for the surfaces that need one rather than
 * a verdict — the muted conversation's quiet Accept (`docs/ux/calls.md`
 * section 2.1.1) among them.
 *
 * Same discipline as `fakeRules`: it records the age it was asked about and
 * answers what the test scripted, so this file cannot pass by re-implementing
 * `remaining_ring_ms`.
 */
function fakeWindow(leftMs: number): RingWindow & { readonly asked: bigint[] } {
  const asked: bigint[] = [];
  return {
    asked,
    remainingRingMs: (ageMs) => {
      asked.push(ageMs);
      return BigInt(leftMs);
    },
  };
}

describe("offerExpiresAtMs", () => {
  it("counts from what is LEFT of the window, not from the whole of it", () => {
    // The distinction is the whole reason this exists. An offer that reached
    // this device 20 s old has 25 s left; a surface armed with the full window
    // from arrival would still be offering an answer after the caller had
    // given up and filed a missed call.
    const window = fakeWindow(25_000);
    expect(offerExpiresAtMs(window, NOW, 20_000)).toBe(NOW + 25_000);
    expect(window.asked).toEqual([20_000n]);
  });

  it("gives an exhausted window a deadline that has already passed", () => {
    const window = fakeWindow(0);
    expect(offerExpiresAtMs(window, NOW, 90_000)).toBe(NOW);
  });

  it("hands the age across the same way the verdict does", () => {
    // One age, two questions: a negative reading is clamped and a fractional
    // one truncated, or the two answers could disagree about the same offer.
    const window = fakeWindow(45_000);
    offerExpiresAtMs(window, NOW, -5_000);
    offerExpiresAtMs(window, NOW, 1_500.75);
    expect(window.asked).toEqual([0n, 1_500n]);
  });
});
