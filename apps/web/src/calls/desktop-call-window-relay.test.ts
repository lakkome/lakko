// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// @vitest-environment jsdom

/**
 * The relay's clock and lock (`desktop-call-window-relay.ts`): ticks come
 * from a worker, the page's own timer stands in when there is none, and the
 * Web Lock is held while relaying and released on stop.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const posted: unknown[] = [];
vi.mock("../platform/host", () => ({
  postCallWindowView: vi.fn(async (view: unknown) => {
    posted.push(view);
  }),
  postCallWindowFrame: vi.fn(async () => true),
}));

const { startCallWindowRelay } = await import("./desktop-call-window-relay");

class FakeWorker {
  static made: FakeWorker[] = [];
  onmessage: ((e: MessageEvent) => void) | null = null;
  onerror: ((e: Event) => void) | null = null;
  started: unknown = null;
  terminated = false;
  constructor(readonly url: URL, readonly opts: WorkerOptions) {
    FakeWorker.made.push(this);
  }
  postMessage(data: unknown): void {
    this.started = data;
  }
  terminate(): void {
    this.terminated = true;
  }
  tick(): void {
    this.onmessage?.(new MessageEvent("message", { data: "tick" }));
  }
}

/** `startCallWindowRelay` ticks once at start; each tick reads the view the
 *  same number of times, so ticks are counted in units of that first read. */
function relay() {
  const root = document.createElement("div");
  let reads = 0;
  const stop = startCallWindowRelay({
    root,
    view: () => {
      reads += 1;
      return null;
    },
    pictureIsScreen: () => false,
    pictureMirrored: () => false,
  });
  const perTick = reads;
  return { stop, ticks: () => reads / perTick };
}

describe("call window relay clock", () => {
  beforeEach(() => {
    FakeWorker.made = [];
    posted.length = 0;
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it("ticks from a worker at a third of the frame interval and stops it", () => {
    vi.stubGlobal("Worker", FakeWorker);
    const { stop, ticks } = relay();
    const w = FakeWorker.made[0];
    expect(w).toBeDefined();
    expect(w?.opts).toEqual({ type: "module" });
    expect(w?.started).toEqual({ every: 33 });
    const before = ticks();
    w?.tick();
    w?.tick();
    expect(ticks()).toBe(before + 2);
    stop();
    expect(w?.terminated).toBe(true);
    w?.tick();
    expect(ticks()).toBe(before + 2);
    expect(posted.at(-1)).toBeNull();
  });

  it("falls back to the page's timer when the worker fails to load", () => {
    vi.useFakeTimers();
    vi.stubGlobal("Worker", FakeWorker);
    const { stop, ticks } = relay();
    const w = FakeWorker.made[0];
    w?.onerror?.(new Event("error"));
    expect(w?.terminated).toBe(true);
    const before = ticks();
    vi.advanceTimersByTime(99);
    expect(ticks()).toBe(before + 3);
    stop();
    vi.advanceTimersByTime(1000);
    expect(ticks()).toBe(before + 3);
  });

  it("uses the page's timer where workers cannot be made", () => {
    vi.useFakeTimers();
    vi.stubGlobal(
      "Worker",
      class {
        constructor() {
          throw new Error("no workers");
        }
      },
    );
    const { stop, ticks } = relay();
    const before = ticks();
    vi.advanceTimersByTime(66);
    expect(ticks()).toBe(before + 2);
    stop();
  });

  it("holds a Web Lock while relaying and lets it go on stop", async () => {
    vi.stubGlobal("Worker", FakeWorker);
    const got: { held: Promise<unknown> | null; name: string | null } = { held: null, name: null };
    vi.stubGlobal("navigator", {
      locks: {
        request: (n: string, cb: () => Promise<unknown>) => {
          got.name = n;
          got.held = cb();
          return got.held;
        },
      },
    });
    const { stop } = relay();
    expect(got.name).toBe("lakko-call-window");
    let done = false;
    void got.held?.then(() => {
      done = true;
    });
    await Promise.resolve();
    expect(done).toBe(false);
    stop();
    await Promise.resolve();
    await Promise.resolve();
    expect(done).toBe(true);
  });
});
