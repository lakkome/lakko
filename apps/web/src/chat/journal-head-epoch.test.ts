// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  JOURNAL_HEAD_PAGE_SIZE,
  scanJournal,
  type JournalHeadPage,
} from "./journal-head-epoch";

// A row's payload is one marker byte the visitor can match on.
const row = (id: number, marker: number) => ({ id, payload: new Uint8Array([marker]) });

describe("scanJournal", () => {
  it("pages to the end, moving the cursor to each page's last id", async () => {
    const pages: JournalHeadPage[] = [
      { messages: [row(1, 1), row(2, 0), row(3, 2)], hasMore: true },
      { messages: [row(4, 2), row(5, 0)], hasMore: true },
      { messages: [row(6, 7)], hasMore: false },
    ];
    const calls: Array<[number, number]> = [];
    const seen: number[] = [];
    const hit = await scanJournal<never>(
      async (after, limit) => {
        calls.push([after, limit]);
        return pages.shift() ?? { messages: [], hasMore: false };
      },
      (m) => {
        seen.push(m.id);
        return undefined;
      },
    );
    expect(hit).toBeUndefined();
    expect(seen).toEqual([1, 2, 3, 4, 5, 6]);
    expect(calls).toEqual([
      [0, JOURNAL_HEAD_PAGE_SIZE],
      [3, JOURNAL_HEAD_PAGE_SIZE],
      [5, JOURNAL_HEAD_PAGE_SIZE],
    ]);
  });

  it("stops on a page that does not move the cursor", async () => {
    let calls = 0;
    await scanJournal<never>(
      async () => {
        calls += 1;
        return { messages: [row(4, 1)], hasMore: true };
      },
      () => undefined,
    );
    expect(calls).toBe(2);
  });

  it("propagates a fetch failure", async () => {
    await expect(
      scanJournal(async () => {
        throw new Error("offline");
      }, () => undefined),
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
