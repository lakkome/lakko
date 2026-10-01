// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { detectGrowth, SNAPSHOT_MAX_DELAY_MS, snapshotDelayMs } from "./growth";

describe("detectGrowth", () => {
  it("sees a new device and names the prior members' leaves", () => {
    const g = detectGrowth(
      [
        { leaf: 2, key: "b" },
        { leaf: 0, key: "a" },
      ],
      [
        { leaf: 0, key: "a" },
        { leaf: 2, key: "b" },
        { leaf: 1, key: "c" },
      ],
    );
    expect(g).toEqual({ grew: true, priorLeaves: [0, 2] });
  });

  it("sees a newcomer on a vacated leaf (keyed on the key, not the leaf)", () => {
    const g = detectGrowth(
      [
        { leaf: 0, key: "a" },
        { leaf: 1, key: "old" },
      ],
      [
        { leaf: 0, key: "a" },
        { leaf: 1, key: "new" },
      ],
    );
    expect(g.grew).toBe(true);
    // The removed device is not a candidate: it can send nothing.
    expect(g.priorLeaves).toEqual([0]);
  });

  it("does not fire on a removal or on nothing", () => {
    expect(detectGrowth([{ leaf: 0, key: "a" }, { leaf: 1, key: "b" }], [{ leaf: 0, key: "a" }]).grew).toBe(
      false,
    );
    expect(detectGrowth([{ leaf: 0, key: "a" }], [{ leaf: 0, key: "a" }]).grew).toBe(false);
  });
});

describe("snapshotDelayMs", () => {
  it("stays within [0, 30 s)", () => {
    expect(snapshotDelayMs(() => 0)).toBe(0);
    expect(snapshotDelayMs(() => 0.5)).toBe(15_000);
    expect(snapshotDelayMs(() => 1)).toBeLessThan(SNAPSHOT_MAX_DELAY_MS);
    expect(snapshotDelayMs(() => Number.NaN)).toBe(0);
    expect(snapshotDelayMs(() => -3)).toBe(0);
  });
});
