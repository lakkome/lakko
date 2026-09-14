// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §2 trigger 3 — growth is a set difference over signature keys.
 *
 * Discrimination (verified): a count comparison fails "a remove and an add in
 * one commit is still growth" with `expected false to be true`.
 */
import { describe, expect, it } from "vitest";

import { membersGrew } from "./membership-growth";

const set = (...keys: string[]): Set<string> => new Set(keys);

describe("membersGrew", () => {
  it("a new key is growth", () => {
    expect(membersGrew(set("a", "b"), set("a", "b", "c"))).toBe(true);
  });

  it("a remove and an add in one commit is still growth", () => {
    // Same count, different people — a count comparison would miss it.
    expect(membersGrew(set("a", "b"), set("a", "c"))).toBe(true);
  });

  it("a shrink or no change is not growth", () => {
    expect(membersGrew(set("a", "b"), set("a"))).toBe(false);
    expect(membersGrew(set("a", "b"), set("b", "a"))).toBe(false);
  });

  it("the first observation is a baseline, not growth", () => {
    expect(membersGrew(null, set("a", "b"))).toBe(false);
  });

  it("a torn roster read is not growth", () => {
    expect(membersGrew(set("a"), set())).toBe(false);
  });
});
