// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0101 walk 2026-11-26, D4 - `history-served.ts`. */
import { describe, expect, it } from "vitest";

import {
  HISTORY_SERVED_WAIT_MS,
  historyTransferDue,
  historyTransferDueFor,
  markHistoryServed,
  readHistoryServed,
} from "./history-served";

function memory(): { getItem(k: string): string | null; setItem(k: string, v: string): void; removeItem(k: string): void } {
  const m = new Map<string, string>();
  return {
    getItem: (k) => m.get(k) ?? null,
    setItem: (k, v) => void m.set(k, v),
    removeItem: (k) => void m.delete(k),
  };
}

describe("historyTransferDue", () => {
  it("follows the table", () => {
    expect(historyTransferDue(undefined, 5)).toBe(false);
    expect(historyTransferDue({ firstHeldMs: 0, served: true }, 1)).toBe(true);
    expect(historyTransferDue({ firstHeldMs: 0, served: false }, HISTORY_SERVED_WAIT_MS - 1)).toBe(false);
    expect(historyTransferDue({ firstHeldMs: 0, served: false }, HISTORY_SERVED_WAIT_MS)).toBe(true);
    // A clock that went back reads as just held back.
    expect(historyTransferDue({ firstHeldMs: 10_000, served: false }, 9_000)).toBe(false);
  });
});

describe("the per-account record", () => {
  it("the first ask starts the wait; a served history makes it due", () => {
    const s = memory();
    expect(historyTransferDueFor("A", "S", 1_000, s)).toBe(false);
    expect(readHistoryServed("A", s).S).toEqual({ firstHeldMs: 1_000, served: false });
    expect(historyTransferDueFor("A", "S", 2_000, s)).toBe(false);
    markHistoryServed("A", "S", 3_000, s);
    expect(readHistoryServed("A", s).S).toEqual({ firstHeldMs: 1_000, served: true });
    expect(historyTransferDueFor("A", "S", 3_001, s)).toBe(true);
  });

  it("is due once the wait ran out, served or not", () => {
    const s = memory();
    historyTransferDueFor("A", "S", 0, s);
    expect(historyTransferDueFor("A", "S", HISTORY_SERVED_WAIT_MS, s)).toBe(true);
  });
});
