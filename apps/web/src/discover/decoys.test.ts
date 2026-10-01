// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  currentDecoys,
  monthKey,
  pickedKey,
  planBucketRounds,
  refreshDue,
  REFRESH_INTERVAL_MS,
  ROUND_SIZE,
  shuffled,
  type DecoyDraw,
} from "./decoys";

/** A deterministic source in [0, 1). */
function seeded(seed: number): () => number {
  let x = seed;
  return () => {
    x = (x * 1103515245 + 12345) % 2147483648;
    return x / 2147483648;
  };
}

const ALL = Array.from({ length: 24 }, (_, i) => i);
const SEPT = Date.UTC(2026, 8, 29);
const OCT = Date.UTC(2026, 9, 1);

describe("monthly decoy draw", () => {
  it("is reused within a month and language", () => {
    const first = currentDecoys(null, ALL, "en", SEPT, seeded(1));
    expect(first.fresh).toBe(true);
    expect(first.draw.month).toBe("2026-09");
    expect([...first.draw.order].sort((a, b) => a - b)).toEqual(ALL);
    const again = currentDecoys(first.draw, ALL, "en", SEPT + 24 * 3600 * 1000, seeded(99));
    expect(again.fresh).toBe(false);
    expect(again.draw).toBe(first.draw);
  });

  it("is drawn again in a new month, or for another language", () => {
    const first = currentDecoys(null, ALL, "en", SEPT, seeded(1)).draw;
    expect(currentDecoys(first, ALL, "en", OCT, seeded(2)).fresh).toBe(true);
    expect(currentDecoys(first, ALL, "de", SEPT, seeded(2)).fresh).toBe(true);
    expect(monthKey(OCT)).toBe("2026-10");
  });
});

describe("bucket rounds", () => {
  const draw: DecoyDraw = { month: "2026-09", language: "en", order: shuffled(ALL, seeded(7)) };

  it("pads picked topics with decoys to eight, and the same decoys every day", () => {
    const a = planBucketRounds([3, 5], draw, true, seeded(1));
    const b = planBucketRounds([3, 5], draw, true, seeded(2));
    expect(a).toHaveLength(1);
    expect(a[0]).toHaveLength(ROUND_SIZE);
    expect(a[0]).toContain(3);
    expect(a[0]).toContain(5);
    expect(new Set(a[0]).size).toBe(ROUND_SIZE);
    // Shuffled differently, but the same set: repetition does not separate
    // picked buckets from decoys.
    expect([...(a[0] as number[])].sort((x, y) => x - y)).toEqual([...(b[0] as number[])].sort((x, y) => x - y));
  });

  it("never uses a picked topic as a decoy", () => {
    const picked = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    const rounds = planBucketRounds(picked, draw, true, seeded(3));
    expect(rounds).toHaveLength(2);
    for (const r of rounds) expect(r).toHaveLength(ROUND_SIZE);
    const all = rounds.flat();
    for (const p of picked) expect(all.filter((t) => t === p)).toHaveLength(1);
  });

  it("drops the padding when mixing is off", () => {
    const rounds = planBucketRounds([4, 2], draw, false, seeded(3));
    expect(rounds).toHaveLength(1);
    expect([...(rounds[0] as number[])].sort()).toEqual([2, 4]);
  });

  it("fetches nothing with no picked topics", () => {
    expect(planBucketRounds([], draw, true, seeded(3))).toEqual([]);
  });
});

describe("refresh cadence", () => {
  it("is once a day, or at once when the picks change", () => {
    const key = pickedKey([2, 1], "en");
    expect(key).toBe(pickedKey([1, 2, 2], "en"));
    expect(refreshDue(null, null, key, SEPT)).toBe(true);
    expect(refreshDue(SEPT, key, key, SEPT + 1000)).toBe(false);
    expect(refreshDue(SEPT, key, key, SEPT + REFRESH_INTERVAL_MS)).toBe(true);
    expect(refreshDue(SEPT, key, pickedKey([1], "en"), SEPT + 1000)).toBe(true);
    // A clock that went backwards refreshes rather than waiting forever.
    expect(refreshDue(SEPT, key, key, SEPT - 1000)).toBe(true);
  });
});
