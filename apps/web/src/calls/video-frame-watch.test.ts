// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import {
  BindingFrameGate,
  canWatchVideoFrames,
  DecodedFrameCounter,
  FRAME_STATS_POLL_MS,
  framesDecodedOf,
  presentedFrameOf,
  registerVideoFrameSource,
  watchDecodedFrames,
  watchVideoFrames,
  type FrameStatsDeps,
  type FrameStatsSource,
  type PresentedFrame,
} from "./video-frame-watch";

/** A receiver whose report says `decoded.value` frames were decoded. */
function fakeSource(decoded: { value: number | null }, calls = { n: 0 }): FrameStatsSource {
  return {
    async getStats() {
      calls.n += 1;
      const entries: unknown[] = [{ type: "codec", id: "C" }];
      if (decoded.value !== null) {
        entries.push({ type: "inbound-rtp", kind: "video", framesDecoded: decoded.value });
      }
      return { values: () => entries };
    },
  };
}

/** A hand-driven timer and clock. */
function fakeDeps() {
  let fn: (() => void) | null = null;
  let ms = 0;
  let cleared = false;
  let now = 0;
  const deps: FrameStatsDeps = {
    setInterval(f, interval) {
      fn = f;
      ms = interval;
      return 7;
    },
    clearInterval(handle) {
      if (handle === 7) cleared = true;
    },
    now: () => now,
  };
  return {
    deps,
    /** One poll at time `t`, with its answer delivered. */
    async tick(t: number): Promise<void> {
      now = t;
      fn?.();
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    },
    interval: () => ms,
    cleared: () => cleared,
  };
}

describe("framesDecodedOf", () => {
  it("reads the video inbound stream's count", () => {
    expect(
      framesDecodedOf([
        { type: "inbound-rtp", kind: "audio", framesDecoded: 9 },
        { type: "outbound-rtp", kind: "video", framesDecoded: 8 },
        { type: "inbound-rtp", kind: "video", framesDecoded: 41 },
      ]),
    ).toBe(41);
    // An older engine's name for the kind.
    expect(framesDecodedOf([{ type: "inbound-rtp", mediaType: "video", framesDecoded: 3 }])).toBe(3);
  });

  it("is null when the report has no count", () => {
    expect(framesDecodedOf([])).toBeNull();
    expect(framesDecodedOf([{ type: "inbound-rtp", kind: "video" }])).toBeNull();
    expect(framesDecodedOf([{ type: "inbound-rtp", kind: "video", framesDecoded: "12" }])).toBeNull();
    expect(framesDecodedOf([null, 4, "x"])).toBeNull();
  });
});

describe("DecodedFrameCounter", () => {
  it("reports a frame only when the count grew since the last reading", () => {
    const c = new DecodedFrameCounter();
    expect(c.observe(120)).toBe(false); // a starting point, not a frame
    expect(c.observe(120)).toBe(false);
    expect(c.observe(123)).toBe(true);
    expect(c.observe(123)).toBe(false);
    expect(c.observe(null)).toBe(false);
    expect(c.observe(124)).toBe(true);
  });

  it("starts again when the count goes backwards or is reset", () => {
    const c = new DecodedFrameCounter();
    c.observe(50);
    expect(c.observe(2)).toBe(false);
    expect(c.observe(3)).toBe(true);
    c.reset();
    expect(c.observe(10)).toBe(false);
  });
});

describe("watchDecodedFrames (the statistics path)", () => {
  it("asks every 50 ms and reports a frame, at the clock's time, when framesDecoded grew", async () => {
    const decoded = { value: 10 as number | null };
    const t = fakeDeps();
    const frames: number[] = [];
    watchDecodedFrames(() => fakeSource(decoded), (now) => frames.push(now), t.deps);
    expect(FRAME_STATS_POLL_MS).toBe(50);
    expect(t.interval()).toBe(50);
    const source = fakeSource(decoded);
    const stop = watchDecodedFrames(() => source, (now) => frames.push(now), t.deps);

    await t.tick(100); // the starting point
    expect(frames).toEqual([]);
    decoded.value = 13;
    await t.tick(200);
    expect(frames).toEqual([200]);
    await t.tick(300); // stands still
    await t.tick(400);
    expect(frames).toEqual([200]);
    decoded.value = 14;
    await t.tick(500);
    expect(frames).toEqual([200, 500]);

    stop();
    expect(t.cleared()).toBe(true);
    decoded.value = 99;
    await t.tick(600);
    expect(frames).toEqual([200, 500]);
  });

  it("reports nothing without a receiver, and starts again when the receiver changes", async () => {
    const a = { value: 100 as number | null };
    const b = { value: 5 as number | null };
    const sourceA = fakeSource(a);
    const sourceB = fakeSource(b);
    let current: FrameStatsSource | undefined;
    const t = fakeDeps();
    const frames: number[] = [];
    watchDecodedFrames(() => current, (now) => frames.push(now), t.deps);

    await t.tick(100);
    expect(frames).toEqual([]);
    current = sourceA;
    await t.tick(200);
    a.value = 101;
    await t.tick(300);
    expect(frames).toEqual([300]);
    // Another receiver, with a lower count: a starting point, not a frame.
    current = sourceB;
    await t.tick(400);
    expect(frames).toEqual([300]);
    b.value = 6;
    await t.tick(500);
    expect(frames).toEqual([300, 500]);
  });

  it("does not ask again while the last question is unanswered, and survives a refusal", async () => {
    let release: ((v: { values(): Iterable<unknown> }) => void) | null = null;
    let asked = 0;
    let fail = false;
    const source: FrameStatsSource = {
      getStats() {
        asked += 1;
        if (fail) return Promise.reject(new Error("InvalidStateError"));
        return new Promise((resolve) => {
          release = resolve;
        });
      },
    };
    const t = fakeDeps();
    const frames: number[] = [];
    watchDecodedFrames(() => source, (now) => frames.push(now), t.deps);
    await t.tick(100);
    await t.tick(200);
    await t.tick(300);
    expect(asked).toBe(1);
    (release as unknown as (v: { values(): Iterable<unknown> }) => void)({
      values: () => [{ type: "inbound-rtp", kind: "video", framesDecoded: 1 }],
    });
    await Promise.resolve();
    await Promise.resolve();
    await t.tick(400);
    expect(asked).toBe(2);
    fail = true;
    (release as unknown as (v: { values(): Iterable<unknown> }) => void)({ values: () => [] });
    await Promise.resolve();
    await Promise.resolve();
    await t.tick(500);
    await t.tick(600);
    expect(asked).toBe(4);
    expect(frames).toEqual([]);
  });
});

describe("watchVideoFrames (one interface, two ways)", () => {
  it("uses requestVideoFrameCallback where the element has it, and never asks for statistics", () => {
    const callbacks: ((now: number) => void)[] = [];
    const cancel = vi.fn();
    const el = {
      requestVideoFrameCallback(cb: (now: number) => void): number {
        callbacks.push(cb);
        return callbacks.length;
      },
      cancelVideoFrameCallback: cancel,
    };
    const track = {};
    const calls = { n: 0 };
    registerVideoFrameSource(track, fakeSource({ value: 1 }, calls));
    const frames: number[] = [];
    expect(canWatchVideoFrames(el)).toBe(true);
    const stop = watchVideoFrames(el, (now) => frames.push(now), () => track);
    callbacks[0]?.(16);
    callbacks[1]?.(33);
    expect(frames).toEqual([16, 33]);
    expect(callbacks).toHaveLength(3);
    stop();
    expect(cancel).toHaveBeenCalledWith(3);
    callbacks[2]?.(50);
    expect(frames).toEqual([16, 33]);
    expect(calls.n).toBe(0);
  });

  it("falls back to the registered receiver's statistics where the element has no callback", async () => {
    vi.useFakeTimers();
    try {
      const el = {};
      const track = {};
      const decoded = { value: 7 as number | null };
      const frames: number[] = [];
      let playing: object | undefined;
      const trackOf = (): object | undefined => playing;

      // No track yet, then a track nobody registered: nothing to watch.
      expect(canWatchVideoFrames(el)).toBe(false);
      expect(canWatchVideoFrames(el, trackOf)).toBe(false);
      playing = track;
      expect(canWatchVideoFrames(el, trackOf)).toBe(false);
      registerVideoFrameSource(track, fakeSource(decoded));
      expect(canWatchVideoFrames(el, trackOf)).toBe(true);

      const stop = watchVideoFrames(el, (now) => frames.push(now), trackOf);
      await vi.advanceTimersByTimeAsync(100);
      expect(frames).toEqual([]);
      decoded.value = 9;
      await vi.advanceTimersByTimeAsync(100);
      expect(frames).toHaveLength(1);
      await vi.advanceTimersByTimeAsync(300);
      expect(frames).toHaveLength(1);
      stop();
      decoded.value = 20;
      await vi.advanceTimersByTimeAsync(300);
      expect(frames).toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("does nothing with neither", () => {
    const frames: number[] = [];
    const stop = watchVideoFrames({}, (now) => frames.push(now));
    stop();
    expect(frames).toEqual([]);
  });
});

describe("presentedFrameOf", () => {
  it("reads the arrival time and RTP timestamp of the callback's metadata", () => {
    expect(
      presentedFrameOf({ receiveTime: 1234.5, rtpTimestamp: 90000, presentedFrames: 3, width: 640 }),
    ).toEqual({ receiveTime: 1234.5, rtpTimestamp: 90000 });
  });

  it("leaves out what the browser did not give, or gave as something other than a number", () => {
    expect(presentedFrameOf(undefined)).toEqual({});
    expect(presentedFrameOf(null)).toEqual({});
    expect(presentedFrameOf({ width: 640 })).toEqual({ receiveTime: undefined, rtpTimestamp: undefined });
    expect(presentedFrameOf({ receiveTime: "1", rtpTimestamp: Number.NaN })).toEqual({
      receiveTime: undefined,
      rtpTimestamp: undefined,
    });
  });
});

describe("BindingFrameGate (a frame decoded before the bind is not the new picture's)", () => {
  it("admits every frame before anything was bound", () => {
    const g = new BindingFrameGate();
    expect(g.awaiting()).toBe(false);
    expect(g.admit({})).toBe(true);
    expect(g.admit({ receiveTime: 1, rtpTimestamp: 1 })).toBe(true);
  });

  it("refuses the track's old last frame Firefox shows at once, and admits the first new one", () => {
    // The numbers of the 2026-11-07 loopback run in Firefox 159: bound at
    // 7092, the leftover was received 5040 ms before, the first new frame
    // 1503 ms after the bind once the sender was resumed.
    const g = new BindingFrameGate();
    g.bind(7092);
    expect(g.awaiting()).toBe(true);
    expect(g.admit({ receiveTime: 7092 - 5040, rtpTimestamp: 449125856 })).toBe(false);
    expect(g.awaiting()).toBe(true);
    expect(g.admit({ receiveTime: 7092 + 1503, rtpTimestamp: 449711576 })).toBe(true);
    expect(g.awaiting()).toBe(false);
    expect(g.admit({ receiveTime: 7092 + 1544, rtpTimestamp: 449717606 })).toBe(true);
  });

  it("admits the first frame at once when it arrived after the bind (Chromium, WebKit)", () => {
    const g = new BindingFrameGate();
    g.bind(1000);
    expect(g.admit({ receiveTime: 1000, rtpTimestamp: 7 })).toBe(true);
    expect(g.awaiting()).toBe(false);
  });

  it("refuses the leftover again if it is presented again", () => {
    const g = new BindingFrameGate();
    g.bind(1000);
    expect(g.admit({ receiveTime: 10, rtpTimestamp: 42 })).toBe(false);
    expect(g.admit({ receiveTime: 10, rtpTimestamp: 42 })).toBe(false);
    expect(g.admit({ receiveTime: 1040, rtpTimestamp: 43 })).toBe(true);
  });

  it("holds back at most one frame whatever the clocks say", () => {
    // An arrival time on a clock that runs behind the page's would make
    // every frame look older than the bind; only the first is held back.
    const g = new BindingFrameGate();
    g.bind(1_000_000);
    expect(g.admit({ receiveTime: 5, rtpTimestamp: 1 })).toBe(false);
    expect(g.admit({ receiveTime: 6, rtpTimestamp: 2 })).toBe(true);
    expect(g.admit({ receiveTime: 7, rtpTimestamp: 3 })).toBe(true);
  });

  it("treats a first frame of unknown arrival as a leftover, and admits the next", () => {
    const g = new BindingFrameGate();
    g.bind(100);
    const unknown: PresentedFrame = {};
    expect(g.admit(unknown)).toBe(false);
    expect(g.admit(unknown)).toBe(true);
  });

  it("starts again at every bind", () => {
    const g = new BindingFrameGate();
    g.bind(100);
    expect(g.admit({ receiveTime: 150, rtpTimestamp: 1 })).toBe(true);
    g.bind(5000);
    expect(g.awaiting()).toBe(true);
    // The frame received under the previous binding is now the leftover.
    expect(g.admit({ receiveTime: 150, rtpTimestamp: 1 })).toBe(false);
    expect(g.admit({ receiveTime: 5030, rtpTimestamp: 2 })).toBe(true);
  });
});

describe("watchVideoFrames hands on what the browser said of each frame", () => {
  it("passes the callback metadata through, and nothing on the statistics path", async () => {
    const callbacks: ((now: number, meta?: unknown) => void)[] = [];
    const el = {
      requestVideoFrameCallback(cb: (now: number, meta?: unknown) => void): number {
        callbacks.push(cb);
        return callbacks.length;
      },
      cancelVideoFrameCallback: (): void => {},
    };
    const seen: [number, PresentedFrame][] = [];
    const stop = watchVideoFrames(el, (now, frame) => seen.push([now, frame]));
    callbacks[0]?.(16, { receiveTime: 12, rtpTimestamp: 3000, width: 320 });
    callbacks[1]?.(33);
    stop();
    expect(seen).toEqual([
      [16, { receiveTime: 12, rtpTimestamp: 3000 }],
      [33, {}],
    ]);
  });
});
