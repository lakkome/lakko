// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0073 R.7 case 1 - the durable re-send cursor of `continuity-store.ts`. */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  advanceResendCursor,
  clearContinuityStateForAccount,
  CONTINUITY_STATE_PREFIX,
  markResent,
  resendCursorOf,
  wasResent,
} from "./continuity-store";
import { resendRemaining } from "./reset-repair";

class MemoryStorage {
  private map = new Map<string, string>();
  getItem(k: string): string | null {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, String(v));
  }
  removeItem(k: string): void {
    this.map.delete(k);
  }
}

const ACCOUNT = "YWNjb3VudA==";
const OLD = "b2xkLWdyb3Vw";

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
});
afterEach(() => {
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
});

describe("the re-send cursor", () => {
  it("starts at none", () => {
    expect(resendCursorOf(ACCOUNT, OLD)).toBeNull();
  });

  it("records the last message that landed, per old group, durably", () => {
    advanceResendCursor(ACCOUNT, OLD, 5);
    expect(resendCursorOf(ACCOUNT, OLD)).toBe(5);
    expect(resendCursorOf(ACCOUNT, "another")).toBeNull();
    expect(resendCursorOf("b3RoZXI=", OLD)).toBeNull();
    // What a reload reads is what was written.
    expect(JSON.parse(localStorage.getItem(CONTINUITY_STATE_PREFIX + ACCOUNT) ?? "{}").resendCursor).toEqual({
      [OLD]: 5,
    });
  });

  it("never moves back", () => {
    advanceResendCursor(ACCOUNT, OLD, 8);
    advanceResendCursor(ACCOUNT, OLD, 3);
    expect(resendCursorOf(ACCOUNT, OLD)).toBe(8);
  });

  it("refuses an id that is not one", () => {
    advanceResendCursor(ACCOUNT, OLD, -1);
    advanceResendCursor(ACCOUNT, OLD, Number.NaN);
    advanceResendCursor(ACCOUNT, OLD, 1.5);
    expect(resendCursorOf(ACCOUNT, OLD)).toBeNull();
  });

  it("an interrupted re-send continues after the last message that landed, never from the first", () => {
    const ids = [3, 5, 8, 13];
    const sent: number[] = [];
    // First pass: the third send fails.
    try {
      for (const id of resendRemaining(ids, resendCursorOf(ACCOUNT, OLD))) {
        if (id === 8) throw new Error("the network went away");
        sent.push(id);
        advanceResendCursor(ACCOUNT, OLD, id);
      }
    } catch {
      /* interrupted */
    }
    expect(wasResent(ACCOUNT, OLD)).toBe(false);
    // Second pass, as after a reload.
    for (const id of resendRemaining(ids, resendCursorOf(ACCOUNT, OLD))) {
      sent.push(id);
      advanceResendCursor(ACCOUNT, OLD, id);
    }
    markResent(ACCOUNT, OLD);
    expect(sent).toEqual([3, 5, 8, 13]);
    expect(wasResent(ACCOUNT, OLD)).toBe(true);
  });

  it("a row written before the cursor existed reads as no cursor", () => {
    localStorage.setItem(CONTINUITY_STATE_PREFIX + ACCOUNT, JSON.stringify({ moved: {}, resent: {} }));
    expect(resendCursorOf(ACCOUNT, OLD)).toBeNull();
    advanceResendCursor(ACCOUNT, OLD, 2);
    expect(resendCursorOf(ACCOUNT, OLD)).toBe(2);
  });

  it("is forgotten with the account's data", () => {
    advanceResendCursor(ACCOUNT, OLD, 5);
    clearContinuityStateForAccount(ACCOUNT);
    expect(resendCursorOf(ACCOUNT, OLD)).toBeNull();
  });
});
