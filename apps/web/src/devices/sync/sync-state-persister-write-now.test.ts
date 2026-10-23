// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * {@link SyncStatePersister.writeNow}: the write an invite link's record
 * waits for before the link is handed out (2026-10-22). A page closed
 * within the debounce had forgotten the link, and every request through it
 * waited for ever.
 */
import { describe, expect, it } from "vitest";

import { SyncStatePersister, type CapturedSyncState } from "./sync-state-persister";

function row(tag: number): CapturedSyncState {
  return { row: { synced: new Uint8Array([tag]), pending: [] }, generation: 1 };
}

/** Timers that never fire on their own: a debounced write happens only when
 *  the test says so. */
function manualTimers(): {
  setTimer: (fn: () => void, ms: number) => unknown;
  clearTimer: (h: unknown) => void;
  pending: () => number;
} {
  const live = new Set<unknown>();
  return {
    setTimer: (fn) => {
      const h = { fn };
      live.add(h);
      return h;
    },
    clearTimer: (h) => {
      live.delete(h);
    },
    pending: () => live.size,
  };
}

describe("SyncStatePersister.writeNow", () => {
  it("writes at once, without waiting for the debounce", async () => {
    const saved: number[] = [];
    const timers = manualTimers();
    const p = new SyncStatePersister({
      debounceMs: 500,
      save: async (_a, c) => {
        saved.push(c.row.synced[0] ?? -1);
      },
      ...timers,
    });
    await p.writeNow("acct", () => row(7));
    expect(saved).toEqual([7]);
  });

  it("replaces a change waiting for its debounce with the newest capture", async () => {
    const saved: number[] = [];
    const timers = manualTimers();
    const p = new SyncStatePersister({
      debounceMs: 500,
      save: async (_a, c) => {
        saved.push(c.row.synced[0] ?? -1);
      },
      ...timers,
    });
    p.schedule("acct", () => row(1));
    expect(timers.pending()).toBe(1);
    await p.writeNow("acct", () => row(2));
    expect(timers.pending()).toBe(0);
    expect(p.hasQueued).toBe(false);
    expect(saved).toEqual([2]);
  });

  it("rejects when the save fails", async () => {
    const reported: unknown[] = [];
    const p = new SyncStatePersister({
      debounceMs: 500,
      save: async () => {
        throw new Error("disk full");
      },
      onError: (err) => reported.push(err),
      ...manualTimers(),
    });
    await expect(p.writeNow("acct", () => row(3))).rejects.toThrow("disk full");
    expect(reported).toHaveLength(1);
  });

  it("rejects when the account is no longer bound", async () => {
    const saved: number[] = [];
    const p = new SyncStatePersister({
      debounceMs: 500,
      save: async (_a, c) => {
        saved.push(c.row.synced[0] ?? -1);
      },
      ...manualTimers(),
    });
    await expect(p.writeNow("acct", () => null)).rejects.toThrow();
    expect(saved).toEqual([]);
  });

  it("lands after a write already running, never under it", async () => {
    const saved: number[] = [];
    let release: (() => void) | null = null;
    const p = new SyncStatePersister({
      debounceMs: 500,
      save: async (_a, c) => {
        if (c.row.synced[0] === 1) await new Promise<void>((r) => (release = r));
        saved.push(c.row.synced[0] ?? -1);
      },
      ...manualTimers(),
    });
    p.schedule("acct", () => row(1));
    const first = p.flush();
    const second = p.writeNow("acct", () => row(2));
    while (release === null) await Promise.resolve();
    expect(saved).toEqual([]);
    (release as () => void)();
    await first;
    await second;
    expect(saved).toEqual([1, 2]);
  });
});
