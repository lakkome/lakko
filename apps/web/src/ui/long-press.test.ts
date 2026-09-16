// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createLongPress, LONG_PRESS_MS, movedPastTolerance } from "./long-press";

function pointer(
  type: string,
  x: number,
  y: number,
  isPrimary = true,
): PointerEvent {
  return { pointerType: type, clientX: x, clientY: y, isPrimary } as unknown as PointerEvent;
}

function click(): MouseEvent & { prevented: boolean } {
  const e = { prevented: false } as MouseEvent & { prevented: boolean };
  (e as { preventDefault: () => void }).preventDefault = () => {
    e.prevented = true;
  };
  (e as { stopPropagation: () => void }).stopPropagation = () => undefined;
  return e;
}

describe("movedPastTolerance", () => {
  it("is a distance check, not per axis", () => {
    expect(movedPastTolerance(5, 5, 8)).toBe(false);
    expect(movedPastTolerance(6, 6, 8)).toBe(true);
    expect(movedPastTolerance(9, 0, 8)).toBe(true);
  });
});

describe("createLongPress", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("fires after the delay for a still touch and swallows the click after it", () => {
    const fired: [number, number][] = [];
    const h = createLongPress((x, y) => fired.push([x, y]));
    h.onPointerDown(pointer("touch", 10, 20));
    vi.advanceTimersByTime(LONG_PRESS_MS - 1);
    expect(fired).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(fired).toEqual([[10, 20]]);
    const c = click();
    expect(h.swallowClick(c)).toBe(true);
    expect(c.prevented).toBe(true);
    // Only once.
    expect(h.swallowClick(click())).toBe(false);
  });

  it("a moving touch is a scroll and never fires", () => {
    const fired: number[] = [];
    const h = createLongPress(() => fired.push(1));
    h.onPointerDown(pointer("touch", 10, 20));
    h.onPointerMove(pointer("touch", 10, 40));
    vi.advanceTimersByTime(LONG_PRESS_MS + 10);
    expect(fired).toEqual([]);
    expect(h.swallowClick(click())).toBe(false);
  });

  it("a release before the delay cancels", () => {
    const fired: number[] = [];
    const h = createLongPress(() => fired.push(1));
    h.onPointerDown(pointer("pen", 0, 0));
    vi.advanceTimersByTime(200);
    h.onPointerUp(pointer("pen", 0, 0));
    vi.advanceTimersByTime(LONG_PRESS_MS);
    expect(fired).toEqual([]);
  });

  it("ignores the mouse and a secondary pointer", () => {
    const fired: number[] = [];
    const h = createLongPress(() => fired.push(1));
    h.onPointerDown(pointer("mouse", 0, 0));
    h.onPointerDown(pointer("touch", 0, 0, false));
    vi.advanceTimersByTime(LONG_PRESS_MS + 10);
    expect(fired).toEqual([]);
  });
});
