// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** The knock read floor: at most one read of a group's knocks per 10 s. */
import { beforeEach, describe, expect, it } from "vitest";

import {
  KNOCK_READ_MIN_INTERVAL_MS,
  resetKnockReadSlotsForTests,
  takeKnockReadSlot,
} from "./knock-cursor";

beforeEach(() => resetKnockReadSlotsForTests());

describe("takeKnockReadSlot", () => {
  it("allows one read per group per 10 s, however often the poll returns", () => {
    expect(KNOCK_READ_MIN_INTERVAL_MS).toBe(10_000);
    const t0 = 1_000_000;
    expect(takeKnockReadSlot("g", t0)).toBe(true);
    // A burst of short journal fetches (two a second, the rate iOS was
    // measured at) gets no further read inside the window.
    for (let t = t0 + 500; t < t0 + 10_000; t += 500) {
      expect(takeKnockReadSlot("g", t)).toBe(false);
    }
    expect(takeKnockReadSlot("g", t0 + 10_000)).toBe(true);
    expect(takeKnockReadSlot("g", t0 + 10_001)).toBe(false);
  });

  it("each group has its own window", () => {
    expect(takeKnockReadSlot("a", 5)).toBe(true);
    expect(takeKnockReadSlot("b", 6)).toBe(true);
    expect(takeKnockReadSlot("a", 7)).toBe(false);
  });

  it("a clock that went backwards reads as due, so it can never stop knock reads", () => {
    expect(takeKnockReadSlot("g", 50_000)).toBe(true);
    expect(takeKnockReadSlot("g", 40_000)).toBe(true);
  });
});
