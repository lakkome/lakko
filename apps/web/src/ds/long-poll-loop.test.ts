// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Scheduling rules of the automatic receive loop (`long-poll-loop.ts`).
 *
 * Everything time-shaped is faked: `sleep` records the requested delay and
 * resolves at once, `now` advances only when a tick says it took time. That
 * keeps the tests about WHICH delay the loop chooses, not about timers.
 */
import { describe, expect, it } from "vitest";

import {
  backoffFor,
  createWakeLatch,
  MAX_BACKOFF_MS,
  MIN_TICK_SPACING_MS,
  NETWORK_BACKOFF_MS,
  OTHER_BACKOFF_MS,
  runLongPollLoop,
  sleepCancellable,
  type LongPollLoopDeps,
  type PollFailureKind,
} from "./long-poll-loop";

type Step =
  | { kind: "ok"; tookMs?: number; delayMs?: number }
  | { kind: "fail"; failure: PollFailureKind };

/** A harness that plays `steps` in order, then aborts the loop. */
function harness(
  steps: Step[],
  overrides: Partial<LongPollLoopDeps> = {},
): {
  controller: AbortController;
  deps: LongPollLoopDeps;
  sleeps: number[];
  failures: number[];
  ticks: () => number;
} {
  const controller = new AbortController();
  const sleeps: number[] = [];
  const failures: number[] = [];
  let clock = 0;
  let index = 0;
  const deps: LongPollLoopDeps = {
    shouldContinue: () => true,
    isPaused: () => false,
    waitUntilResumed: async () => undefined,
    tick: async () => {
      const step = steps[index];
      index += 1;
      if (step === undefined) {
        controller.abort();
        throw new Error("aborted by harness");
      }
      if (step.kind === "fail") {
        const err = new Error(step.failure);
        throw err;
      }
      clock += step.tookMs ?? 10_000;
      return step.delayMs ?? 0;
    },
    classifyFailure: (err) =>
      (err as Error).message === "network" ? "network" : "other",
    sleep: async (ms) => {
      sleeps.push(ms);
    },
    now: () => clock,
    onFailure: (_err, backoffMs) => {
      failures.push(backoffMs);
    },
    ...overrides,
  };
  return { controller, deps, sleeps, failures, ticks: () => index };
}

describe("backoffFor", () => {
  it("starts at the base for the failure kind and doubles to the cap", () => {
    expect(backoffFor("network", 1)).toBe(NETWORK_BACKOFF_MS);
    expect(backoffFor("network", 2)).toBe(NETWORK_BACKOFF_MS * 2);
    expect(backoffFor("other", 1)).toBe(OTHER_BACKOFF_MS);
    expect(backoffFor("other", 2)).toBe(OTHER_BACKOFF_MS * 2);
    expect(backoffFor("other", 50)).toBe(MAX_BACKOFF_MS);
    expect(backoffFor("network", 0)).toBe(NETWORK_BACKOFF_MS);
  });
});

describe("runLongPollLoop", () => {
  it("re-arms immediately after a successful long poll", async () => {
    const h = harness([{ kind: "ok" }, { kind: "ok" }, { kind: "ok" }]);
    await runLongPollLoop(h.controller.signal, h.deps);
    // Three successful ticks, then the harness aborts on the fourth.
    expect(h.ticks()).toBe(4);
    // A parked request that took seconds needs no extra wait before the next.
    expect(h.sleeps).toEqual([]);
  });

  it("never spins: an instant round trip is spaced by the floor", async () => {
    const h = harness([{ kind: "ok", tookMs: 0 }, { kind: "ok", tookMs: 100 }]);
    await runLongPollLoop(h.controller.signal, h.deps);
    expect(h.sleeps).toEqual([MIN_TICK_SPACING_MS, MIN_TICK_SPACING_MS - 100]);
  });

  it("honours an extra delay the tick asks for", async () => {
    const h = harness([{ kind: "ok", delayMs: 2_000 }]);
    await runLongPollLoop(h.controller.signal, h.deps);
    expect(h.sleeps).toEqual([2_000]);
  });

  it("backs off on failure, doubling per consecutive failure, and resets on success", async () => {
    const h = harness([
      { kind: "fail", failure: "network" },
      { kind: "fail", failure: "network" },
      { kind: "fail", failure: "other" },
      { kind: "ok" },
      { kind: "fail", failure: "other" },
    ]);
    await runLongPollLoop(h.controller.signal, h.deps);
    const expected = [
      NETWORK_BACKOFF_MS,
      NETWORK_BACKOFF_MS * 2,
      OTHER_BACKOFF_MS * 4,
      OTHER_BACKOFF_MS,
    ];
    expect(h.failures).toEqual(expected);
    expect(h.sleeps).toEqual(expected);
  });

  it("does not report or back off a failure caused by the abort itself", async () => {
    const h = harness([]);
    await runLongPollLoop(h.controller.signal, h.deps);
    expect(h.failures).toEqual([]);
    expect(h.sleeps).toEqual([]);
  });

  it("stops for good when shouldContinue turns false", async () => {
    let allowed = 2;
    const h = harness([{ kind: "ok" }, { kind: "ok" }, { kind: "ok" }], {
      shouldContinue: () => allowed > 0,
    });
    const tick = h.deps.tick;
    const deps: LongPollLoopDeps = {
      ...h.deps,
      tick: async (signal) => {
        allowed -= 1;
        return tick(signal);
      },
    };
    await runLongPollLoop(h.controller.signal, deps);
    expect(h.ticks()).toBe(2);
    expect(h.controller.signal.aborted).toBe(false);
  });

  it("does not tick while paused, and ticks again once resumed", async () => {
    let paused = true;
    const latch = createWakeLatch();
    const h = harness([{ kind: "ok" }], {
      isPaused: () => paused,
      waitUntilResumed: (signal) => latch.wait(signal),
    });
    const done = runLongPollLoop(h.controller.signal, h.deps);
    await Promise.resolve();
    await Promise.resolve();
    expect(h.ticks()).toBe(0);
    // A wake while still paused must not produce a tick either.
    latch.wake();
    await Promise.resolve();
    await Promise.resolve();
    expect(h.ticks()).toBe(0);
    paused = false;
    latch.wake();
    await done;
    expect(h.ticks()).toBe(2);
  });

  it("an abort releases a paused loop", async () => {
    const latch = createWakeLatch();
    const h = harness([], {
      isPaused: () => true,
      waitUntilResumed: (signal) => latch.wait(signal),
    });
    const done = runLongPollLoop(h.controller.signal, h.deps);
    h.controller.abort();
    await done;
    expect(h.ticks()).toBe(0);
  });
});

describe("sleepCancellable", () => {
  it("resolves early when the signal aborts", async () => {
    const controller = new AbortController();
    const started = Date.now();
    const pending = sleepCancellable(60_000, controller.signal);
    controller.abort();
    await pending;
    expect(Date.now() - started).toBeLessThan(1_000);
  });
});
