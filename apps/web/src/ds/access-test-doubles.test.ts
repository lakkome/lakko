// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0081 — the shared commit-route double (`withCommitRoute`) behaves as
 * the DS's `POST .../commit` does, so the suites that replay a journal built
 * through it (the send-epoch straddle oracle, the sweep, the direct commits)
 * see exactly the rows the DS would have appended:
 *
 * - the registration and the row land together, and a refusal of either
 *   leaves neither;
 * - an identical re-send appends nothing and answers the original id;
 * - another commit at a registered epoch is a 409 and appends nothing.
 */
import { describe, expect, it, vi } from "vitest";

import { withCommitRoute } from "./access-test-doubles";
import { isCommitConflict } from "./ownership";

const GID = new Uint8Array([7, 7]);

function route(opts: { refuseRegistration?: boolean } = {}) {
  const journal: Uint8Array[] = [];
  const ds = withCommitRoute({
    registerGroupEpoch: vi.fn(async () => {
      if (opts.refuseRegistration === true) throw Object.assign(new Error("401"), { status: 401 });
    }),
    publishGroup: vi.fn(async (_g: Uint8Array, row: Uint8Array) => {
      journal.push(row);
      return { id: journal.length, createdAt: "t" };
    }),
  });
  return { ds, journal };
}

describe("withCommitRoute models the DS commit route", () => {
  it("appends the row with its registration", async () => {
    const { ds, journal } = route();
    const answer = await ds.commitGroupEpoch(GID, 6n, new Uint8Array([1]), [], new Uint8Array([0xc1]));
    expect(answer).toEqual({ id: 1, createdAt: "t", resent: false });
    expect(journal).toEqual([new Uint8Array([0xc1])]);
  });

  it("a refused registration appends no row and leaves the epoch free", async () => {
    const refused = route({ refuseRegistration: true });
    await expect(
      refused.ds.commitGroupEpoch(GID, 6n, new Uint8Array([1]), [], new Uint8Array([0xc1])),
    ).rejects.toThrow("401");
    expect(refused.journal).toEqual([]);
  });

  it("an identical re-send answers the original id and appends nothing", async () => {
    const { ds, journal } = route();
    await ds.commitGroupEpoch(GID, 6n, new Uint8Array([1]), [], new Uint8Array([0xc1]));
    const again = await ds.commitGroupEpoch(GID, 6n, new Uint8Array([1]), [], new Uint8Array([0xc1]));
    expect(again).toEqual({ id: 1, createdAt: "t", resent: true });
    expect(journal).toHaveLength(1);
  });

  it("another commit at a registered epoch is a 409 and appends nothing", async () => {
    const { ds, journal } = route();
    await ds.commitGroupEpoch(GID, 6n, new Uint8Array([1]), [], new Uint8Array([0xc1]));
    const lost = ds.commitGroupEpoch(GID, 6n, new Uint8Array([2]), [], new Uint8Array([0xc2]));
    await expect(lost).rejects.toSatisfy(isCommitConflict);
    expect(journal).toEqual([new Uint8Array([0xc1])]);
  });
});
