// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { drainOutbox, hasPendingFolderSync, type OutboxBatch } from "./folder-outbox";

const batch = (hlc: bigint): OutboxBatch => [{ t: "removed", hlc, folderIdHex: "00".repeat(16) }];

describe("the offline pending line", () => {
  it("shows while anything is waiting, and only then", () => {
    expect(hasPendingFolderSync([])).toBe(false);
    expect(hasPendingFolderSync([batch(1n)])).toBe(true);
  });
});

describe("drainOutbox", () => {
  it("sends oldest first, with the stamps the edits were given", async () => {
    const sent: bigint[] = [];
    const r = await drainOutbox([batch(1n), batch(2n)], async (b) => {
      sent.push(b[0]?.hlc ?? -1n);
    });
    expect(sent).toEqual([1n, 2n]);
    expect(r).toEqual({ remaining: [], sent: 2, failed: false });
  });

  it("stops at the first failure and keeps it and everything after", async () => {
    const q = [batch(1n), batch(2n), batch(3n)];
    let calls = 0;
    const r = await drainOutbox(q, async () => {
      calls += 1;
      if (calls === 2) throw new Error("offline");
    });
    expect(r.failed).toBe(true);
    expect(r.sent).toBe(1);
    expect(r.remaining).toEqual([q[1], q[2]]);
  });

  it("drops a batch that can never be sent, and goes on", async () => {
    const q = [batch(1n), batch(2n)];
    const r = await drainOutbox(
      q,
      async (b) => {
        if (b[0]?.hlc === 1n) throw new Error("bound");
      },
      (err) => err instanceof Error && err.message === "bound",
    );
    expect(r).toEqual({ remaining: [], sent: 1, failed: false });
  });
});
