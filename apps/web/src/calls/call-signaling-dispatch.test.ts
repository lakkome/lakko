// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Inbound call-signaling delivery: exactly once, in order, one at a time; the
 * C-11 route table; the stale-offer gate.
 */
import { describe, expect, it } from "vitest";

import {
  callIdHex,
  isStaleOffer,
  routeSignalingKind,
  SerialOnceQueue,
  STALE_OFFER_MS,
} from "./call-signaling-dispatch";

interface Row {
  readonly messageId: number;
  readonly label: string;
}

function deferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve: () => void = () => {};
  const promise = new Promise<void>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

describe("SerialOnceQueue", () => {
  it("delivers a row once however many times the poll re-feeds it", async () => {
    const seen: string[] = [];
    const q = new SerialOnceQueue<Row>(async (r) => {
      seen.push(r.label);
    });
    // The old effect walked every row on every rows() change.
    const batch = [
      { messageId: 10, label: "offer" },
      { messageId: 11, label: "ice" },
    ];
    for (const r of batch) q.enqueue(r);
    for (const r of batch) q.enqueue(r);
    expect(q.enqueue({ messageId: 10, label: "offer-again" })).toBe(false);
    await q.whenIdle();
    for (const r of batch) q.enqueue(r);
    await q.whenIdle();
    expect(seen).toEqual(["offer", "ice"]);
  });

  it("runs handlers in arrival order and never two at once", async () => {
    const log: string[] = [];
    let running = 0;
    let maxRunning = 0;
    const gate = deferred();
    const q = new SerialOnceQueue<Row>(async (r) => {
      running += 1;
      maxRunning = Math.max(maxRunning, running);
      log.push(`start ${r.label}`);
      // The first handler parks (a capture prompt, an SDP apply) while the
      // later rows are already queued behind it.
      if (r.label === "offer") await gate.promise;
      log.push(`end ${r.label}`);
      running -= 1;
    });
    q.enqueue({ messageId: 1, label: "offer" });
    q.enqueue({ messageId: 2, label: "ice-a" });
    q.enqueue({ messageId: 3, label: "ice-b" });
    await Promise.resolve();
    await Promise.resolve();
    expect(log).toEqual(["start offer"]);
    gate.resolve();
    await q.whenIdle();
    expect(log).toEqual([
      "start offer",
      "end offer",
      "start ice-a",
      "end ice-a",
      "start ice-b",
      "end ice-b",
    ]);
    expect(maxRunning).toBe(1);
  });

  it("a throwing handler is reported and does not wedge the rows behind it", async () => {
    const seen: number[] = [];
    const errors: number[] = [];
    const q = new SerialOnceQueue<Row>(
      async (r) => {
        if (r.messageId === 1) throw new Error("malformed");
        seen.push(r.messageId);
      },
      (_err, r) => errors.push(r.messageId),
    );
    q.enqueue({ messageId: 1, label: "bad" });
    q.enqueue({ messageId: 2, label: "good" });
    await q.whenIdle();
    expect(errors).toEqual([1]);
    expect(seen).toEqual([2]);
  });

  it("close drops what is queued and refuses what comes after", async () => {
    const seen: number[] = [];
    const gate = deferred();
    const q = new SerialOnceQueue<Row>(async (r) => {
      if (r.messageId === 1) await gate.promise;
      seen.push(r.messageId);
    });
    q.enqueue({ messageId: 1, label: "running" });
    q.enqueue({ messageId: 2, label: "queued" });
    // Let the first handler start before closing.
    await Promise.resolve();
    await Promise.resolve();
    q.close();
    expect(q.enqueue({ messageId: 3, label: "late" })).toBe(false);
    gate.resolve();
    await q.whenIdle();
    expect(seen).toEqual([1]);
  });
});

describe("routeSignalingKind (C-11)", () => {
  it("group-control bodies route to the group record only", () => {
    for (const kind of ["groupStart", "groupJoin", "groupLeave"]) {
      expect(routeSignalingKind(kind)).toBe("group");
    }
  });

  it("the 1×1 kinds route to the session and end is shared", () => {
    expect(routeSignalingKind("offer")).toBe("oneToOne");
    expect(routeSignalingKind("answer")).toBe("oneToOne");
    expect(routeSignalingKind("iceCandidate")).toBe("oneToOne");
    expect(routeSignalingKind("end")).toBe("end");
  });

  it("an unknown kind is dropped, not guessed at", () => {
    expect(routeSignalingKind("groupMute")).toBe("drop");
    expect(routeSignalingKind("")).toBe("drop");
  });
});

describe("isStaleOffer", () => {
  const now = Date.parse("2026-09-13T12:00:00.000Z");

  it("rings for a fresh offer", () => {
    expect(isStaleOffer("2026-09-13T11:59:30.000Z", now)).toBe(false);
  });

  it("does not ring for an offer older than the window", () => {
    expect(
      isStaleOffer(new Date(now - STALE_OFFER_MS - 1).toISOString(), now),
    ).toBe(true);
    expect(isStaleOffer(new Date(now - STALE_OFFER_MS).toISOString(), now)).toBe(
      false,
    );
  });

  it("does not ring when the row's age cannot be read", () => {
    expect(isStaleOffer("not a timestamp", now)).toBe(true);
  });
});

describe("callIdHex", () => {
  it("is lowercase, zero-padded hex", () => {
    expect(callIdHex(new Uint8Array([0, 1, 0xab, 0xff]))).toBe("0001abff");
  });
});

describe("SerialOnceQueue fed by several groups (the app-level call host)", () => {
  interface GroupRow {
    readonly groupIdB64: string;
    readonly messageId: number;
  }

  it("keys on (group, messageId): the same DS id in two journals is two rows", async () => {
    const seen: string[] = [];
    const q = new SerialOnceQueue<GroupRow>(
      async (r) => {
        seen.push(`${r.groupIdB64}:${r.messageId}`);
      },
      undefined,
      (r) => `${r.groupIdB64}:${r.messageId}`,
    );
    expect(q.enqueue({ groupIdB64: "A", messageId: 42 })).toBe(true);
    // A DS message id is only unique within one group's journal: row 42 of B
    // is an offer in its own right, not a repeat of row 42 of A.
    expect(q.enqueue({ groupIdB64: "B", messageId: 42 })).toBe(true);
    // The same row handed over by both receive paths (the chat view's poll and
    // the background receive) is still delivered once.
    expect(q.enqueue({ groupIdB64: "A", messageId: 42 })).toBe(false);
    await q.whenIdle();
    expect(seen).toEqual(["A:42", "B:42"]);
  });
});
