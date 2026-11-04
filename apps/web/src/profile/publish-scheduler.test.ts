// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §2 — when a profile is published into a group: on join and on
 * change at once, on growth throttled to one per group per 30 s with 0..10 s of
 * jitter, and coalesced.
 *
 * Time, timers and randomness are fakes owned by the test, so every delay is
 * asserted exactly.
 *
 * ## Discrimination (verified by running the broken variants)
 *
 *   - Throttle removed: "at most one republish per group per 30 s" fails with
 *     `expected 1005000 to be 1030000`.
 *   - Coalescing removed: "a burst of growth coalesces" fails with 5 timers.
 *   - Jitter removed: "a first growth waits only for its jitter" fails with
 *     `expected 1000000 to be 1005000`.
 */
import { describe, expect, it } from "vitest";

import {
  GROWTH_JITTER_MAX_MS,
  GROWTH_MIN_INTERVAL_MS,
  MAX_RETRIES,
  ProfilePublishScheduler,
  type PublishReason,
} from "./publish-scheduler";

interface FakeTimer {
  readonly at: number;
  readonly fn: () => void;
  cancelled: boolean;
}

function harness(opts: { random?: number; fail?: (g: string, reason: PublishReason) => boolean } = {}) {
  let now = 1_000_000;
  const timers: FakeTimer[] = [];
  const published: Array<{ group: string; reason: PublishReason; at: number }> = [];
  const scheduler = new ProfilePublishScheduler<FakeTimer>({
    now: () => now,
    setTimer: (fn, ms) => {
      const t: FakeTimer = { at: now + ms, fn, cancelled: false };
      timers.push(t);
      return t;
    },
    clearTimer: (t) => {
      t.cancelled = true;
    },
    random: () => opts.random ?? 0,
    publish: async (group, reason) => {
      published.push({ group, reason, at: now });
      if (opts.fail?.(group, reason) === true) throw new Error("ds down");
    },
  });
  /** Advance the clock to `to`, firing due timers in order. */
  async function advanceTo(to: number): Promise<void> {
    for (;;) {
      const due = timers
        .filter((t) => !t.cancelled && t.at <= to)
        .sort((a, b) => a.at - b.at)[0];
      if (due === undefined) break;
      due.cancelled = true;
      now = due.at;
      due.fn();
      await Promise.resolve();
      await Promise.resolve();
    }
    now = to;
  }
  return {
    scheduler,
    published,
    timers,
    advanceTo,
    now: () => now,
    pending: () => timers.filter((t) => !t.cancelled),
  };
}

describe("join and change publish immediately", () => {
  it("publishes on join", async () => {
    const h = harness();
    await h.scheduler.joined("g1");
    expect(h.published).toEqual([{ group: "g1", reason: "join", at: h.now() }]);
  });

  it("publishes a change into every group", async () => {
    const h = harness();
    await h.scheduler.profileChanged(["g1", "g2"]);
    expect(h.published.map((p) => [p.group, p.reason])).toEqual([
      ["g1", "change"],
      ["g2", "change"],
    ]);
  });
});

describe("growth is throttled and jittered per group", () => {
  it("a first growth waits only for its jitter", async () => {
    const h = harness({ random: 0.5 });
    h.scheduler.groupGrew("g1");
    expect(h.published).toHaveLength(0);
    const expected = Math.floor(0.5 * (GROWTH_JITTER_MAX_MS + 1));
    expect(h.pending()[0]?.at).toBe(h.now() + expected);
    await h.advanceTo(h.now() + expected);
    expect(h.published.map((p) => p.reason)).toEqual(["growth"]);
  });

  it("jitter stays within 0..10 s", () => {
    for (const random of [0, 0.999999]) {
      const h = harness({ random });
      h.scheduler.groupGrew("g1");
      const delay = (h.pending()[0]?.at ?? 0) - h.now();
      expect(delay).toBeGreaterThanOrEqual(0);
      expect(delay).toBeLessThanOrEqual(GROWTH_JITTER_MAX_MS);
    }
  });

  it("at most one republish per group per 30 s", async () => {
    const h = harness({ random: 0 });
    const start = h.now();
    h.scheduler.groupGrew("g1");
    await h.advanceTo(start);
    expect(h.published).toHaveLength(1);

    // A second growth 5 s later waits for the 30 s window from the first.
    await h.advanceTo(start + 5_000);
    h.scheduler.groupGrew("g1");
    expect(h.pending()[0]?.at).toBe(start + GROWTH_MIN_INTERVAL_MS);
    await h.advanceTo(start + GROWTH_MIN_INTERVAL_MS - 1);
    expect(h.published).toHaveLength(1);
    await h.advanceTo(start + GROWTH_MIN_INTERVAL_MS);
    expect(h.published).toHaveLength(2);
  });

  it("a burst of growth coalesces into one republish", async () => {
    const h = harness({ random: 0.3 });
    for (let i = 0; i < 5; i++) h.scheduler.groupGrew("g1");
    expect(h.pending()).toHaveLength(1);
    await h.advanceTo(h.now() + GROWTH_JITTER_MAX_MS);
    expect(h.published).toHaveLength(1);
  });

  it("the throttle is per group", async () => {
    const h = harness({ random: 0 });
    h.scheduler.groupGrew("g1");
    h.scheduler.groupGrew("g2");
    await h.advanceTo(h.now());
    expect(h.published.map((p) => p.group).sort()).toEqual(["g1", "g2"]);
  });

  it("a publish that goes out cancels a pending growth republish", async () => {
    const h = harness({ random: 0.9 });
    h.scheduler.groupGrew("g1");
    expect(h.scheduler.hasPending("g1")).toBe(true);
    await h.scheduler.profileChanged(["g1"]);
    expect(h.scheduler.hasPending("g1")).toBe(false);
    await h.advanceTo(h.now() + GROWTH_MIN_INTERVAL_MS + GROWTH_JITTER_MAX_MS);
    expect(h.published.map((p) => p.reason)).toEqual(["change"]);
  });

  it("a join counts toward the growth window", async () => {
    const h = harness({ random: 0 });
    const start = h.now();
    await h.scheduler.joined("g1");
    h.scheduler.groupGrew("g1");
    expect(h.pending()[0]?.at).toBe(start + GROWTH_MIN_INTERVAL_MS);
  });
});

describe("failures", () => {
  it("retries a failed publish a bounded number of times", async () => {
    const h = harness({ random: 0, fail: () => true });
    await h.scheduler.joined("g1");
    await h.advanceTo(h.now() + 10 * (GROWTH_MIN_INTERVAL_MS + GROWTH_JITTER_MAX_MS));
    expect(h.published).toHaveLength(1 + MAX_RETRIES);
    expect(h.published.slice(1).every((p) => p.reason === "retry")).toBe(true);
  });

  it("stop cancels everything", async () => {
    const h = harness({ random: 0 });
    h.scheduler.groupGrew("g1");
    h.scheduler.stop();
    await h.advanceTo(h.now() + GROWTH_MIN_INTERVAL_MS);
    expect(h.published).toHaveLength(0);
    await h.scheduler.joined("g1");
    expect(h.published).toHaveLength(0);
  });
});

describe("a profile owed to a group goes out before this device's next message (beforeSend)", () => {
  it("a waiting growth republish is published at once, with no throttle and no jitter", async () => {
    const h = harness({ random: 0.99 });
    await h.scheduler.joined("g1");
    h.scheduler.groupGrew("g1");
    expect(h.scheduler.hasPending("g1")).toBe(true);
    const at = h.now();
    await h.scheduler.beforeSend("g1");
    expect(h.published.map((p) => [p.group, p.reason, p.at])).toEqual([
      ["g1", "join", at],
      ["g1", "growth", at],
    ]);
    expect(h.scheduler.hasPending("g1")).toBe(false);
    // The timer it replaced never fires a second publish.
    await h.advanceTo(at + GROWTH_MIN_INTERVAL_MS + GROWTH_JITTER_MAX_MS);
    expect(h.published).toHaveLength(2);
  });

  it("a failed join publish waiting for its retry is the one sent", async () => {
    let failing = true;
    const h = harness({ fail: () => failing });
    await h.scheduler.joined("g1");
    expect(h.scheduler.hasPending("g1")).toBe(true);
    failing = false;
    await h.scheduler.beforeSend("g1");
    expect(h.published.map((p) => p.reason)).toEqual(["join", "retry"]);
    expect(h.scheduler.hasPending("g1")).toBe(false);
  });

  it("waits for a publish already on its way", async () => {
    let release!: () => void;
    const order: string[] = [];
    const scheduler = new ProfilePublishScheduler<number>({
      now: () => 0,
      setTimer: () => 0,
      clearTimer: () => undefined,
      random: () => 0,
      publish: () =>
        new Promise<void>((resolve) => {
          release = () => {
            order.push("profile");
            resolve();
          };
        }),
    });
    const joining = scheduler.joined("g1");
    const waiting = scheduler.beforeSend("g1").then(() => order.push("send"));
    await Promise.resolve();
    expect(order).toEqual([]);
    release();
    await Promise.all([joining, waiting]);
    expect(order).toEqual(["profile", "send"]);
  });

  it("nothing owed, another group, or stopped: no publish", async () => {
    const h = harness();
    await h.scheduler.beforeSend("g1");
    h.scheduler.groupGrew("g2");
    await h.scheduler.beforeSend("g1");
    expect(h.published).toHaveLength(0);
    h.scheduler.stop();
    await h.scheduler.beforeSend("g2");
    expect(h.published).toHaveLength(0);
  });

  it("the idle republish keeps its jitter", async () => {
    const h = harness({ random: 0.5 });
    h.scheduler.groupGrew("g1");
    expect(h.pending()[0]?.at).toBe(h.now() + Math.floor(0.5 * (GROWTH_JITTER_MAX_MS + 1)));
  });
});
