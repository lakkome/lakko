// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The geometry of dragging a pin (`docs/ux/navigation.md` 2.2.1): where a
 * drop lands, how far the other pins move aside, when the list scrolls.
 */
import { describe, expect, it } from "vitest";

import {
  AUTOSCROLL_MAX_PX,
  autoscrollStep,
  DRAG_START_PX,
  dropIndex,
  movedPast,
  pitchOf,
  shiftOf,
  type Slot,
} from "./pin-drag";

/** Four 60px rows, 4px apart, starting at 100. */
const slots: Slot[] = [0, 1, 2, 3].map((i) => ({ top: 100 + i * 64, height: 60 }));
const middle = (i: number): number => 100 + i * 64 + 30;

describe("dropIndex", () => {
  it("stays put while the dragged row has not passed a neighbour's middle", () => {
    expect(dropIndex(slots, 1, middle(1))).toBe(1);
    expect(dropIndex(slots, 1, middle(1) + 20)).toBe(1);
  });

  it("lands past every pin whose middle it crossed", () => {
    expect(dropIndex(slots, 0, middle(2) + 1)).toBe(2);
    expect(dropIndex(slots, 3, middle(0) + 1)).toBe(1);
  });

  it("above the first pin is first, anywhere below the last is last", () => {
    expect(dropIndex(slots, 2, -10_000)).toBe(0);
    expect(dropIndex(slots, 0, 10_000)).toBe(3);
  });

  it("is always a place among the pins", () => {
    for (let from = 0; from < slots.length; from += 1) {
      for (let y = -500; y < 1_000; y += 7) {
        const at = dropIndex(slots, from, y);
        expect(at).toBeGreaterThanOrEqual(0);
        expect(at).toBeLessThan(slots.length);
      }
    }
    expect(dropIndex([], 0, 5)).toBe(0);
  });
});

describe("shiftOf", () => {
  const pitch = pitchOf(slots, 1);

  it("the pitch is a row plus the gap", () => {
    expect(pitch).toBe(64);
  });

  it("moving down, the pins passed move up by one pitch", () => {
    expect([0, 1, 2, 3].map((i) => shiftOf(i, 1, 3, pitch))).toEqual([0, 0, -64, -64]);
  });

  it("moving up, the pins passed move down by one pitch", () => {
    expect([0, 1, 2, 3].map((i) => shiftOf(i, 3, 1, pitch))).toEqual([0, 64, 64, 0]);
  });

  it("nothing moves while the drop is where it started", () => {
    expect([0, 1, 2, 3].map((i) => shiftOf(i, 2, 2, pitch))).toEqual([0, 0, 0, 0]);
  });
});

describe("autoscroll", () => {
  it("scrolls up near the top, down near the bottom, not in between", () => {
    expect(autoscrollStep(102, 100, 700)).toBeLessThan(0);
    expect(autoscrollStep(698, 100, 700)).toBeGreaterThan(0);
    expect(autoscrollStep(400, 100, 700)).toBe(0);
  });

  it("is fastest at and past the edge", () => {
    expect(autoscrollStep(50, 100, 700)).toBe(-AUTOSCROLL_MAX_PX);
    expect(autoscrollStep(800, 100, 700)).toBe(AUTOSCROLL_MAX_PX);
  });
});

describe("movedPast", () => {
  it("a press that stays within the threshold is a click", () => {
    expect(movedPast(3, 3, DRAG_START_PX)).toBe(false);
    expect(movedPast(0, DRAG_START_PX + 1, DRAG_START_PX)).toBe(true);
  });
});
