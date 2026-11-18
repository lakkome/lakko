// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../storage/account-kv", () => ({
  loadAccountJson: vi.fn(async () => null),
  saveAccountJson: vi.fn(async () => undefined),
}));

import {
  BALLOT_DELAY_MAX_MS,
  ballotDelayMs,
  ballotQueue,
  bindBallotQueue,
  drainBallotsForTest,
  dueBallots,
  enqueueBallot,
  nextDueAt,
  queuedBallotOf,
  rescheduledAtOpen,
  resetBallotQueueForTest,
  retried,
  retryBallot,
  withQueued,
  type BallotPorts,
  type QueuedBallot,
} from "./ballot-queue";

const q = (over: Partial<QueuedBallot> = {}): QueuedBallot => ({
  id: "a",
  groupIdB64: "Zw==",
  pollMessageId: 7,
  choices: [1],
  rowB64: "AQID",
  setVersion: 0,
  notBeforeMs: 100,
  state: "waiting",
  ...over,
});

afterEach(() => resetBallotQueueForTest());

describe("the anonymous ballot delay (ADR-0090 section 3.6)", () => {
  it("is uniform over 0..=10 s, both ends reachable", () => {
    expect(ballotDelayMs(0)).toBe(0);
    expect(ballotDelayMs(0.999_999_999_9)).toBe(BALLOT_DELAY_MAX_MS);
    expect(ballotDelayMs(0.5)).toBe(5000);
    for (let i = 0; i < 1000; i++) {
      const d = ballotDelayMs(Math.random());
      expect(d).toBeGreaterThanOrEqual(0);
      expect(d).toBeLessThanOrEqual(BALLOT_DELAY_MAX_MS);
    }
    expect(ballotDelayMs(Number.NaN)).toBe(0);
    expect(ballotDelayMs(2)).toBe(BALLOT_DELAY_MAX_MS);
  });
});

describe("the queue rules", () => {
  it("lets a newer vote in the same poll replace one still waiting", () => {
    const out = withQueued([q({ id: "old", choices: [0] }), q({ id: "other", pollMessageId: 8 })], q({ id: "new", choices: [2] }));
    expect(out.map((x) => x.id)).toEqual(["other", "new"]);
  });

  it("posts nothing before the session landed, and only what is due", () => {
    const list = [q({ id: "due", notBeforeMs: 50 }), q({ id: "later", notBeforeMs: 500 }), q({ id: "failed", notBeforeMs: 10, state: "failed" })];
    expect(dueBallots(list, 100, false)).toEqual([]);
    expect(dueBallots(list, 100, true).map((x) => x.id)).toEqual(["due"]);
    expect(nextDueAt(list)).toBe(50);
  });

  it("retries with the same row and a fresh delay", () => {
    const r = retried(q({ state: "failed" }), 1000, 0.25);
    expect(r.state).toBe("waiting");
    expect(r.rowB64).toBe("AQID");
    expect(r.notBeforeMs).toBe(1000 + 2500);
  });

  it("at the next open, re-delays what waited and keeps what failed", () => {
    const out = rescheduledAtOpen([q({ id: "w", notBeforeMs: 1 }), q({ id: "f", state: "failed", notBeforeMs: 1 })], 5000, () => 0.1);
    expect(out[0]?.notBeforeMs).toBe(5000 + 1000);
    expect(out[1]?.notBeforeMs).toBe(1);
  });
});

describe("the queue runtime", () => {
  function ports(over: Partial<BallotPorts> = {}): BallotPorts & { posted: string[]; recorded: number } {
    const p = {
      posted: [] as string[],
      recorded: 0,
      async post(g: string) {
        p.posted.push(g);
        return 1;
      },
      async recordChoice() {
        p.recorded += 1;
      },
      sessionLanded: () => true,
      now: () => 10_000,
      rand: () => 0,
      ...over,
    };
    return p;
  }

  it("posts a due ballot, then records the own choice and empties the queue", async () => {
    const p = ports();
    await bindBallotQueue("acct", p);
    enqueueBallot({ groupIdB64: "Zw==", pollMessageId: 7, choices: [1], rowB64: "AQID", setVersion: 3 });
    expect(queuedBallotOf("Zw==", 7)?.state).toBe("waiting");
    await drainBallotsForTest();
    expect(p.posted).toEqual(["Zw=="]);
    expect(p.recorded).toBe(1);
    expect(ballotQueue()).toEqual([]);
  });

  it("holds every ballot until the session landed", async () => {
    let landed = false;
    const p = ports({ sessionLanded: () => landed });
    await bindBallotQueue("acct", p);
    enqueueBallot({ groupIdB64: "Zw==", pollMessageId: 7, choices: [1], rowB64: "AQID", setVersion: 0 });
    await drainBallotsForTest();
    expect(p.posted).toEqual([]);
    landed = true;
    await drainBallotsForTest();
    expect(p.posted).toEqual(["Zw=="]);
  });

  it("marks a failed post, and Try again sends it", async () => {
    let fail = true;
    const p = ports({
      async post() {
        if (fail) throw new Error("offline");
        return 2;
      },
    });
    await bindBallotQueue("acct", p);
    enqueueBallot({ groupIdB64: "Zw==", pollMessageId: 7, choices: [1], rowB64: "AQID", setVersion: 0 });
    await drainBallotsForTest();
    expect(queuedBallotOf("Zw==", 7)?.state).toBe("failed");
    fail = false;
    retryBallot("Zw==", 7);
    await drainBallotsForTest();
    expect(queuedBallotOf("Zw==", 7)).toBeUndefined();
  });

  it("schedules a ballot no earlier than its random delay", async () => {
    let now = 10_000;
    const p = ports({ now: () => now, rand: () => 0.5 });
    await bindBallotQueue("acct", p);
    enqueueBallot({ groupIdB64: "Zw==", pollMessageId: 7, choices: [1], rowB64: "AQID", setVersion: 0 });
    expect(queuedBallotOf("Zw==", 7)?.notBeforeMs).toBe(15_000);
    await drainBallotsForTest();
    expect(p.posted).toEqual([]);
    now = 15_000;
    await drainBallotsForTest();
    expect(p.posted).toEqual(["Zw=="]);
  });
});
