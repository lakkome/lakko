// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { atBottom, attachStickToBottom, decideStick, USER_SCROLL_SETTLE_MS } from "./stick-to-bottom";

describe("decideStick", () => {
  const g = (scrollTop: number, scrollHeight: number, clientHeight: number) => ({
    scrollTop,
    scrollHeight,
    clientHeight,
  });

  it("scrolls back to the end when the view got shorter under a held list", () => {
    // Was at the bottom of 1000 in 500; the view is now 400 tall.
    expect(decideStick(true, false, g(500, 1000, 400))).toEqual({ scrollTo: 600, stuck: true });
  });

  it("scrolls back to the end when the last bubble grew", () => {
    expect(decideStick(true, false, g(500, 1200, 500))).toEqual({ scrollTo: 700, stuck: true });
  });

  it("leaves a reader of older messages where they are", () => {
    expect(decideStick(false, false, g(100, 1000, 400))).toEqual({ scrollTo: null, stuck: false });
  });

  it("does nothing while the person scrolls; held again only if they end at the bottom", () => {
    expect(decideStick(true, true, g(300, 1000, 400))).toEqual({ scrollTo: null, stuck: false });
    expect(decideStick(true, true, g(600, 1000, 400))).toEqual({ scrollTo: null, stuck: true });
  });

  it("counts the end within the tolerance as the bottom", () => {
    expect(atBottom(g(599, 1000, 400))).toBe(true);
    expect(atBottom(g(590, 1000, 400))).toBe(false);
  });
});

describe("attachStickToBottom", () => {
  type Callback = () => void;
  let resize: Callback[] = [];

  beforeEach(() => {
    resize = [];
    vi.stubGlobal(
      "ResizeObserver",
      class {
        private readonly cb: Callback;
        constructor(cb: Callback) {
          this.cb = cb;
          resize.push(cb);
        }
        observe(): void {}
        unobserve(): void {}
        disconnect(): void {
          resize = resize.filter((c) => c !== this.cb);
        }
      },
    );
  });
  afterEach(() => vi.unstubAllGlobals());

  /** A scroller whose geometry the test sets. */
  function scroller(init: { scrollTop: number; scrollHeight: number; clientHeight: number }) {
    const el = document.createElement("div");
    const geo = { ...init };
    Object.defineProperty(el, "scrollHeight", { get: () => geo.scrollHeight });
    Object.defineProperty(el, "clientHeight", { get: () => geo.clientHeight });
    Object.defineProperty(el, "scrollTop", {
      get: () => geo.scrollTop,
      set: (v: number) => {
        geo.scrollTop = Math.min(Math.max(0, v), geo.scrollHeight - geo.clientHeight);
      },
    });
    document.body.appendChild(el);
    return { el, geo };
  }
  const fireResize = (): void => {
    for (const cb of resize) cb();
  };

  it("keeps the newest message in view when the pinned bar takes room", () => {
    const { el, geo } = scroller({ scrollTop: 500, scrollHeight: 1000, clientHeight: 500 });
    const stick = attachStickToBottom(el);
    geo.clientHeight = 440; // the pinned bar appeared above the timeline
    fireResize();
    expect(geo.scrollTop).toBe(560);
    // ... and again when the composer grows with a reply strip.
    geo.clientHeight = 400;
    fireResize();
    expect(geo.scrollTop).toBe(600);
    stick.dispose();
  });

  it("leaves someone reading older messages alone, and holds again at the bottom", () => {
    const { el, geo } = scroller({ scrollTop: 500, scrollHeight: 1000, clientHeight: 500 });
    const stick = attachStickToBottom(el);
    geo.scrollTop = 100;
    el.dispatchEvent(new Event("scroll"));
    geo.clientHeight = 400;
    fireResize();
    expect(geo.scrollTop).toBe(100);
    // Back at the end: held again.
    geo.scrollTop = 600;
    el.dispatchEvent(new Event("scroll"));
    geo.scrollHeight = 1100; // a new message
    fireResize();
    expect(geo.scrollTop).toBe(700);
    stick.dispose();
  });

  it("stops holding after a jump to an older message", () => {
    const { el, geo } = scroller({ scrollTop: 500, scrollHeight: 1000, clientHeight: 500 });
    const stick = attachStickToBottom(el);
    stick.release();
    geo.scrollHeight = 1200;
    fireResize();
    expect(geo.scrollTop).toBe(500);
    expect(stick.stuck()).toBe(false);
    stick.dispose();
  });

  it("does not fight the person's own scroll", () => {
    vi.useFakeTimers();
    try {
      const { el, geo } = scroller({ scrollTop: 500, scrollHeight: 1000, clientHeight: 500 });
      const stick = attachStickToBottom(el);
      el.dispatchEvent(new Event("wheel"));
      geo.clientHeight = 400;
      fireResize();
      expect(geo.scrollTop).toBe(500);
      vi.advanceTimersByTime(USER_SCROLL_SETTLE_MS + 1);
      stick.dispose();
    } finally {
      vi.useRealTimers();
    }
  });
});
