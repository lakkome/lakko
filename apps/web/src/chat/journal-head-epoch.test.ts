// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  JOURNAL_HEAD_PAGE_SIZE,
  journalHeadEpoch,
  scanJournal,
  type JournalHeadPage,
} from "./journal-head-epoch";

// A "payload" here is one byte: 0 = not MLS, n = a row after which the group
// is at epoch n. That is exactly the contract of `epochAfterMessage`.
const row = (id: number, epoch: number) => ({ id, payload: new Uint8Array([epoch]) });
const epochAfter = (p: Uint8Array): bigint | undefined => {
  const b = p[0];
  return b === undefined || b === 0 ? undefined : BigInt(b);
};

describe("journalHeadEpoch", () => {
  it("pages to the head instead of trusting the first page", async () => {
    const pages: JournalHeadPage[] = [
      { messages: [row(1, 1), row(2, 0), row(3, 2)], hasMore: true },
      { messages: [row(4, 2), row(5, 0)], hasMore: true },
      { messages: [row(6, 7)], hasMore: false },
    ];
    const calls: Array<[number, number]> = [];
    const epoch = await journalHeadEpoch(async (after, limit) => {
      calls.push([after, limit]);
      return pages.shift() ?? { messages: [], hasMore: false };
    }, epochAfter);
    expect(epoch).toBe(7n);
    expect(calls).toEqual([
      [0, JOURNAL_HEAD_PAGE_SIZE],
      [3, JOURNAL_HEAD_PAGE_SIZE],
      [5, JOURNAL_HEAD_PAGE_SIZE],
    ]);
  });

  it("keeps the last MLS epoch when the head rows are not MLS", async () => {
    const epoch = await journalHeadEpoch(
      async () => ({ messages: [row(1, 4), row(2, 0), row(3, 0)], hasMore: false }),
      epochAfter,
    );
    expect(epoch).toBe(4n);
  });

  it("reads an empty journal as epoch 0", async () => {
    expect(await journalHeadEpoch(async () => ({ messages: [], hasMore: false }), epochAfter)).toBe(0n);
  });

  it("stops when a page does not move the cursor", async () => {
    let calls = 0;
    const epoch = await journalHeadEpoch(async () => {
      calls += 1;
      return { messages: [row(5, 3)], hasMore: true };
    }, epochAfter);
    expect(epoch).toBe(3n);
    expect(calls).toBe(2);
  });

  it("propagates a fetch failure instead of answering with a stale epoch", async () => {
    await expect(
      journalHeadEpoch(async () => {
        throw new Error("offline");
      }, epochAfter),
    ).rejects.toThrow("offline");
  });

  it("scanJournal finds a row on a later page and stops there", async () => {
    const pages: JournalHeadPage[] = [
      { messages: [row(1, 1), row(2, 2)], hasMore: true },
      { messages: [row(3, 9), row(4, 5)], hasMore: true },
      { messages: [row(5, 6)], hasMore: false },
    ];
    let fetched = 0;
    const hit = await scanJournal(
      async () => {
        fetched += 1;
        return pages.shift() ?? { messages: [], hasMore: false };
      },
      (m) => (m.payload[0] === 9 ? m.id : undefined),
    );
    expect(hit).toBe(3);
    expect(fetched).toBe(2);
  });

  it("scanJournal returns undefined when nothing matches", async () => {
    const hit = await scanJournal(
      async () => ({ messages: [row(1, 1)], hasMore: false }),
      () => undefined,
    );
    expect(hit).toBeUndefined();
  });
});
