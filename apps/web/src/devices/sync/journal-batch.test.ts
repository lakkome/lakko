// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  applyDeviceJournalBatch,
  startingCursor,
  type DeviceJournalRow,
} from "./journal-batch";

function rows(...ids: number[]): DeviceJournalRow[] {
  return ids.map((id) => ({ id, payload: new Uint8Array([id & 0xff]) }));
}

/** A fake durable store: the cursor it holds is what a reload would start at. */
function harness(opts: { own?: number[]; persistFails?: boolean; applyThrowsOn?: number[] } = {}) {
  const own = new Set(opts.own ?? []);
  const applied: number[] = [];
  let storedCursor: number | null = null;
  return {
    applied,
    stored: () => storedCursor,
    deps: {
      isOwnEcho: (id: number) => own.has(id),
      apply: async (row: DeviceJournalRow) => {
        applied.push(row.id);
        if (opts.applyThrowsOn?.includes(row.id)) throw new Error("bad row");
      },
      persist: async (cursor: number) => {
        if (opts.persistFails === true) throw new Error("disk full");
        storedCursor = cursor;
      },
    },
  };
}

describe("applyDeviceJournalBatch", () => {
  it("persists the last row id, so a reload starts after the batch rather than at 0", async () => {
    const h = harness();
    const out = await applyDeviceJournalBatch(rows(1, 2, 3), 0, h.deps);
    expect(out.persisted).toBe(true);
    expect(out.nextCursor).toBe(3);
    expect(h.stored()).toBe(3);
    // The next page lifetime resumes here.
    expect(startingCursor(h.stored())).toBe(3);
  });

  it("never hands this device's own publish to the MLS apply", async () => {
    const h = harness({ own: [2] });
    const out = await applyDeviceJournalBatch(rows(1, 2, 3), 0, h.deps);
    expect(h.applied).toEqual([1, 3]);
    expect(out.skippedOwnEchoes).toEqual([2]);
    // ...but the cursor still moves past it.
    expect(out.nextCursor).toBe(3);
  });

  it("never skips a row from another device", async () => {
    // Nothing is registered as own: every row must reach apply, including one
    // whose apply throws (a wrong-epoch frame is still not ours to drop).
    const h = harness({ applyThrowsOn: [5] });
    const out = await applyDeviceJournalBatch(rows(4, 5, 6), 3, h.deps);
    expect(h.applied).toEqual([4, 5, 6]);
    expect(out.skippedOwnEchoes).toEqual([]);
    expect(out.nextCursor).toBe(6);
  });

  it("holds the cursor when the persist fails — it may lag the durable state, never lead", async () => {
    const h = harness({ persistFails: true, own: [8] });
    const out = await applyDeviceJournalBatch(rows(7, 8, 9), 6, h.deps);
    expect(out.persisted).toBe(false);
    expect(out.nextCursor).toBe(6);
    expect(h.stored()).toBeNull();
    // The echo is still reported so the caller keeps its id armed.
    expect(out.skippedOwnEchoes).toEqual([8]);
  });

  it("an empty batch persists nothing and keeps its position", async () => {
    const h = harness();
    const out = await applyDeviceJournalBatch([], 12, h.deps);
    expect(out).toEqual({ nextCursor: 12, persisted: false, skippedOwnEchoes: [], applied: 0 });
    expect(h.stored()).toBeNull();
  });

  it("persists a batch made only of own echoes, so they are not re-read after a reload", async () => {
    const h = harness({ own: [20, 21] });
    const out = await applyDeviceJournalBatch(rows(20, 21), 19, h.deps);
    expect(h.applied).toEqual([]);
    expect(out.persisted).toBe(true);
    expect(h.stored()).toBe(21);
  });
});

describe("startingCursor", () => {
  it("starts at 0 with nothing stored or something unusable", () => {
    expect(startingCursor(null)).toBe(0);
    expect(startingCursor(undefined)).toBe(0);
    expect(startingCursor(-4)).toBe(0);
    expect(startingCursor(1.5)).toBe(0);
    expect(startingCursor(Number.NaN)).toBe(0);
  });

  it("resumes at a stored position", () => {
    expect(startingCursor(42)).toBe(42);
  });
});
