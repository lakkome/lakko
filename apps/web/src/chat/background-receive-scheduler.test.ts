// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  BACKGROUND_IDLE_NO_PUSH_MS,
  BACKGROUND_IDLE_PUSH_MS,
  BACKGROUND_SWEEP_INTERVAL_MS,
  backgroundSweepIntervalMs,
  GroupBackoff,
  groupsAppeared,
  runBackgroundReceiveLoop,
  runSweep,
  sweepPauseMs,
  type BackgroundLoopDeps,
  type SweepDeps,
} from "./background-receive-scheduler";
import { createWakeLatch } from "../ds/long-poll-loop";

function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void } {
  let resolve: (v: T) => void = () => {};
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function sweepDeps(over: Partial<SweepDeps> & Pick<SweepDeps, "poll">): SweepDeps {
  return {
    groups: () => ["a", "b", "c", "d"],
    isExcluded: () => false,
    concurrency: 1,
    maxImmediateRepolls: 4,
    ...over,
  };
}

describe("runSweep", () => {
  it("reports how many polls it made and how many answered", async () => {
    const tallies: Array<{ tried: number; answered: number }> = [];
    await runSweep(
      sweepDeps({
        isExcluded: (id) => id === "d",
        poll: async (id) => {
          if (id === "b") throw new Error("offline");
          return { yieldedRows: false };
        },
        onSweepDone: (t) => tallies.push({ ...t }),
      }),
      new AbortController().signal,
    );
    expect(tallies).toEqual([{ tried: 3, answered: 2 }]);
  });

  it("polls every group once, in round-robin order", async () => {
    const order: string[] = [];
    await runSweep(
      sweepDeps({
        poll: async (id) => {
          order.push(id);
          return { yieldedRows: false };
        },
      }),
      new AbortController().signal,
    );
    expect(order).toEqual(["a", "b", "c", "d"]);
  });

  it("never polls the group whose chat view is open", async () => {
    const order: string[] = [];
    await runSweep(
      sweepDeps({
        isExcluded: (id) => id === "b",
        poll: async (id) => {
          order.push(id);
          return { yieldedRows: false };
        },
      }),
      new AbortController().signal,
    );
    expect(order).toEqual(["a", "c", "d"]);
  });

  it("re-checks exclusion before each poll (a chat opened mid-sweep is left to its view)", async () => {
    const order: string[] = [];
    let open: string | null = null;
    await runSweep(
      sweepDeps({
        isExcluded: (id) => id === open,
        poll: async (id) => {
          order.push(id);
          if (id === "a") open = "c";
          return { yieldedRows: false };
        },
      }),
      new AbortController().signal,
    );
    expect(order).toEqual(["a", "b", "d"]);
  });

  it("keeps at most `concurrency` polls in flight, whatever the group count", async () => {
    let inFlight = 0;
    let maxInFlight = 0;
    const groups = Array.from({ length: 12 }, (_, i) => `g${i}`);
    const polled: string[] = [];
    await runSweep(
      sweepDeps({
        groups: () => groups,
        concurrency: 2,
        poll: async (id) => {
          inFlight += 1;
          maxInFlight = Math.max(maxInFlight, inFlight);
          await new Promise((r) => setTimeout(r, 1));
          polled.push(id);
          inFlight -= 1;
          return { yieldedRows: false };
        },
      }),
      new AbortController().signal,
    );
    expect(maxInFlight).toBe(2);
    expect([...polled].sort()).toEqual([...groups].sort());
  });

  it("re-polls a group that yielded rows immediately, ahead of the rest", async () => {
    const order: string[] = [];
    let aRows = 2;
    await runSweep(
      sweepDeps({
        poll: async (id) => {
          order.push(id);
          if (id === "a" && aRows > 0) {
            aRows -= 1;
            return { yieldedRows: true };
          }
          return { yieldedRows: false };
        },
      }),
      new AbortController().signal,
    );
    expect(order).toEqual(["a", "a", "a", "b", "c", "d"]);
  });

  it("caps immediate re-polls so a busy chat cannot starve the others", async () => {
    const order: string[] = [];
    await runSweep(
      sweepDeps({
        maxImmediateRepolls: 2,
        poll: async (id) => {
          order.push(id);
          return { yieldedRows: id === "a" };
        },
      }),
      new AbortController().signal,
    );
    expect(order).toEqual(["a", "a", "a", "b", "c", "d"]);
  });

  it("never polls one group twice at once", async () => {
    let aInFlight = 0;
    let maxA = 0;
    await runSweep(
      sweepDeps({
        groups: () => ["a"],
        concurrency: 2,
        poll: async (id) => {
          if (id === "a") {
            aInFlight += 1;
            maxA = Math.max(maxA, aInFlight);
          }
          await new Promise((r) => setTimeout(r, 1));
          aInFlight -= 1;
          return { yieldedRows: true };
        },
      }),
      new AbortController().signal,
    );
    expect(maxA).toBe(1);
  });

  it("skips a group in failure backoff and polls it again once the backoff expires", async () => {
    let now = 1_000;
    const backoff = new GroupBackoff();
    const order: string[] = [];
    const deps = sweepDeps({
      groups: () => ["a", "b"],
      backoff,
      now: () => now,
      onPollFailed: (id) => backoff.failed(id, now, "network"),
      poll: async (id) => {
        order.push(id);
        if (id === "a") throw new Error("DS down");
        return { yieldedRows: false };
      },
    });
    await runSweep(deps, new AbortController().signal);
    await runSweep(deps, new AbortController().signal);
    expect(order).toEqual(["a", "b", "b"]);
    now += 60_000;
    await runSweep(deps, new AbortController().signal);
    expect(order).toEqual(["a", "b", "b", "a", "b"]);
  });
});

describe("runBackgroundReceiveLoop", () => {
  function loopDeps(over: Partial<BackgroundLoopDeps>): BackgroundLoopDeps {
    return {
      ...sweepDeps({ poll: async () => ({ yieldedRows: false }) }),
      isActive: () => true,
      waitUntilActive: async () => {},
      sleep: async () => {},
      random: () => 0.5,
      intervalMs: 4_000,
      jitterMs: 1_000,
      ...over,
    };
  }

  it("does not poll while inactive (hidden tab, no call) and resumes on wake", async () => {
    let active = false;
    const latch = createWakeLatch();
    const polled: string[] = [];
    const controller = new AbortController();
    const firstSweep = deferred<void>();
    const loop = runBackgroundReceiveLoop(
      controller.signal,
      loopDeps({
        isActive: () => active,
        waitUntilActive: (signal) => latch.wait(signal),
        sleep: async () => {
          firstSweep.resolve();
          // Park until aborted, so the test observes exactly one sweep.
          await new Promise<void>((r) => controller.signal.addEventListener("abort", () => r()));
        },
        poll: async (id) => {
          polled.push(id);
          return { yieldedRows: false };
        },
      }),
    );
    await new Promise((r) => setTimeout(r, 5));
    expect(polled).toEqual([]);
    active = true;
    latch.wake();
    await firstSweep.promise;
    expect(polled).toEqual(["a", "b", "c", "d"]);
    controller.abort();
    await loop;
  });

  it("stops for good on sign-out (abort), including mid-pause", async () => {
    const controller = new AbortController();
    let sweeps = 0;
    let pauses = 0;
    const loop = runBackgroundReceiveLoop(
      controller.signal,
      loopDeps({
        groups: () => {
          sweeps += 1;
          return ["a"];
        },
        sleep: async (_ms, signal) => {
          pauses += 1;
          if (pauses === 2) controller.abort();
          if (signal.aborted) return;
        },
      }),
    );
    await loop;
    expect(sweeps).toBe(2);
    const after = sweeps;
    await new Promise((r) => setTimeout(r, 5));
    expect(sweeps).toBe(after);
  });

  it("aborting during a sweep stops the remaining polls", async () => {
    const controller = new AbortController();
    const polled: string[] = [];
    await runBackgroundReceiveLoop(
      controller.signal,
      loopDeps({
        poll: async (id) => {
          polled.push(id);
          if (id === "b") controller.abort();
          return { yieldedRows: false };
        },
      }),
    );
    expect(polled).toEqual(["a", "b"]);
  });
});

describe("sweepPauseMs", () => {
  it("is interval plus or minus jitter, never negative", () => {
    expect(sweepPauseMs(4_000, 1_000, 0)).toBe(3_000);
    expect(sweepPauseMs(4_000, 1_000, 0.5)).toBe(4_000);
    expect(sweepPauseMs(4_000, 1_000, 1)).toBe(5_000);
    expect(sweepPauseMs(100, 1_000, 0)).toBe(0);
  });
});

describe("backgroundSweepIntervalMs (an idle client polls no group every few seconds)", () => {
  it("is short only while something is arriving", () => {
    expect(backgroundSweepIntervalMs({ wakeWindowOpen: true, callInProgress: false, pushWake: "reachable" })).toBe(
      BACKGROUND_SWEEP_INTERVAL_MS,
    );
    expect(backgroundSweepIntervalMs({ wakeWindowOpen: false, callInProgress: true, pushWake: "unknown" })).toBe(
      BACKGROUND_SWEEP_INTERVAL_MS,
    );
  });

  it("is long when idle, longest when a push wake reaches the tab", () => {
    expect(backgroundSweepIntervalMs({ wakeWindowOpen: false, callInProgress: false, pushWake: "reachable" })).toBe(
      BACKGROUND_IDLE_PUSH_MS,
    );
    expect(backgroundSweepIntervalMs({ wakeWindowOpen: false, callInProgress: false, pushWake: "unreachable" })).toBe(
      BACKGROUND_IDLE_NO_PUSH_MS,
    );
    expect(BACKGROUND_IDLE_NO_PUSH_MS).toBeGreaterThanOrEqual(30_000);
  });
});

describe("groupsAppeared (walk 7: a joined group waited for the idle sweep)", () => {
  it("says nothing before the first list of a session", () => {
    expect(groupsAppeared(null, ["a", "b"])).toBe(false);
  });

  it("sees a group that was not there", () => {
    expect(groupsAppeared(new Set(["a"]), ["a", "joined"])).toBe(true);
  });

  it("does not count the same groups, or a group that left", () => {
    expect(groupsAppeared(new Set(["a", "b"]), ["b", "a"])).toBe(false);
    expect(groupsAppeared(new Set(["a", "b"]), ["a"])).toBe(false);
  });
});
