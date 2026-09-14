// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { MAX_BUFFERED_EVENTS_PER_GROUP, ReceiveCoordinator } from "./receive-coordinator";

describe("ReceiveCoordinator — the shared cursor", () => {
  it("never moves a group's cursor backward", () => {
    const c = new ReceiveCoordinator<number>();
    expect(c.cursor("g")).toBeUndefined();
    expect(c.noteCursor("g", 10)).toBe(10);
    // The chat view hydrated an older value from the log after the background
    // receive advanced the group: the older value must not win.
    expect(c.noteCursor("g", 4)).toBe(10);
    expect(c.cursor("g")).toBe(10);
    expect(c.noteCursor("g", 12)).toBe(12);
  });

  it("ignores an unusable cursor", () => {
    const c = new ReceiveCoordinator<number>();
    c.noteCursor("g", 5);
    expect(c.noteCursor("g", -1)).toBe(5);
    expect(c.noteCursor("g", Number.NaN)).toBe(5);
  });

  it("keeps groups apart", () => {
    const c = new ReceiveCoordinator<number>();
    c.noteCursor("a", 7);
    expect(c.cursor("b")).toBeUndefined();
  });
});

describe("ReceiveCoordinator — foreground claims", () => {
  it("reference-counts claims so an overlapping mount/unmount keeps the group foreground", () => {
    const c = new ReceiveCoordinator<number>();
    const releaseOld = c.claimForeground("g");
    const releaseNew = c.claimForeground("g");
    releaseOld();
    expect(c.isForeground("g")).toBe(true);
    releaseNew();
    expect(c.isForeground("g")).toBe(false);
  });

  it("a release is idempotent", () => {
    const c = new ReceiveCoordinator<number>();
    const r1 = c.claimForeground("g");
    const r2 = c.claimForeground("g");
    r1();
    r1();
    expect(c.isForeground("g")).toBe(true);
    r2();
    expect(c.isForeground("g")).toBe(false);
  });

  it("notifies listeners on every change", () => {
    const c = new ReceiveCoordinator<number>();
    let n = 0;
    const off = c.onForegroundChange(() => (n += 1));
    const r = c.claimForeground("g");
    r();
    off();
    c.claimForeground("h");
    expect(n).toBe(2);
  });
});

describe("ReceiveCoordinator — event hand-off", () => {
  it("buffers events for a chat that is not open and hands them over, once, on attach", () => {
    const c = new ReceiveCoordinator<number>();
    expect(c.deliver("g", [1, 2])).toBe("buffered");
    expect(c.deliver("g", [3])).toBe("buffered");
    const seen: number[][] = [];
    const detach = c.attachView("g", (e) => seen.push([...e]));
    expect(seen).toEqual([[1, 2, 3]]);
    expect(c.bufferedCount("g")).toBe(0);
    detach();
    // A second view mounted later does not see them again.
    const again: number[][] = [];
    c.attachView("g", (e) => again.push([...e]));
    expect(again).toEqual([]);
  });

  it("delivers straight to an attached view without buffering", () => {
    const c = new ReceiveCoordinator<number>();
    const seen: number[] = [];
    const detach = c.attachView("g", (e) => seen.push(...e));
    expect(c.deliver("g", [9])).toBe("delivered");
    expect(seen).toEqual([9]);
    expect(c.bufferedCount("g")).toBe(0);
    detach();
    expect(c.deliver("g", [10])).toBe("buffered");
  });

  it("a stale detach does not remove a newer view", () => {
    const c = new ReceiveCoordinator<number>();
    const first: number[] = [];
    const second: number[] = [];
    const detachFirst = c.attachView("g", (e) => first.push(...e));
    c.attachView("g", (e) => second.push(...e));
    detachFirst();
    c.deliver("g", [4]);
    expect(second).toEqual([4]);
    expect(first).toEqual([]);
  });

  it("bounds the buffer, dropping the oldest", () => {
    const c = new ReceiveCoordinator<number>();
    const events = Array.from({ length: MAX_BUFFERED_EVENTS_PER_GROUP + 5 }, (_, i) => i);
    c.deliver("g", events);
    expect(c.bufferedCount("g")).toBe(MAX_BUFFERED_EVENTS_PER_GROUP);
    const seen: number[] = [];
    c.attachView("g", (e) => seen.push(...e));
    expect(seen[0]).toBe(5);
  });

  it("reset forgets everything (sign-out)", () => {
    const c = new ReceiveCoordinator<number>();
    c.noteCursor("g", 3);
    c.claimForeground("g");
    c.deliver("h", [1]);
    c.reset();
    expect(c.cursor("g")).toBeUndefined();
    expect(c.isForeground("g")).toBe(false);
    expect(c.bufferedCount("h")).toBe(0);
  });
});
