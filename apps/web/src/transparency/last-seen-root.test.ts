// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * KT-log S3c — last-seen-root no-rollback gate tests.
 *
 * Pure `localStorage` logic (no wasm). Backed by an in-memory
 * `localStorage` shim, mirroring the contacts/local-store tests.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  checkAndAdvance,
  loadLastSeenLogSize,
  clearLastSeenRoot,
} from "./last-seen-root";

/** In-memory `localStorage` shim (vitest `node` env has none). */
class MemoryStorage {
  private map = new Map<string, string>();
  getItem(k: string): string | null {
    return this.map.has(k) ? (this.map.get(k) as string) : null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, String(v));
  }
  removeItem(k: string): void {
    this.map.delete(k);
  }
  clear(): void {
    this.map.clear();
  }
  get length(): number {
    return this.map.size;
  }
  key(i: number): string | null {
    return Array.from(this.map.keys())[i] ?? null;
  }
}

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
});

afterEach(() => {
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
});

describe("checkAndAdvance (no-rollback gate)", () => {
  it("starts at 0 and accepts the first observed size", () => {
    expect(loadLastSeenLogSize()).toBe(0n);
    expect(checkAndAdvance(5n)).toBe("ok");
    expect(loadLastSeenLogSize()).toBe(5n);
  });

  it("advances on a strictly-larger size", () => {
    checkAndAdvance(5n);
    expect(checkAndAdvance(9n)).toBe("ok");
    expect(loadLastSeenLogSize()).toBe(9n);
  });

  it("accepts an equal size idempotently without regressing", () => {
    checkAndAdvance(7n);
    expect(checkAndAdvance(7n)).toBe("ok");
    expect(loadLastSeenLogSize()).toBe(7n);
  });

  it("REFUSES a smaller size as a rollback and never advances", () => {
    checkAndAdvance(10n);
    expect(checkAndAdvance(4n)).toBe("rollback");
    // The stored high-water mark is untouched — NEVER lowered.
    expect(loadLastSeenLogSize()).toBe(10n);
  });

  it("keeps refusing repeated rollbacks, mark stays at the high-water", () => {
    checkAndAdvance(100n);
    expect(checkAndAdvance(99n)).toBe("rollback");
    expect(checkAndAdvance(1n)).toBe("rollback");
    expect(checkAndAdvance(0n)).toBe("rollback");
    expect(loadLastSeenLogSize()).toBe(100n);
    // A later legitimate growth still advances.
    expect(checkAndAdvance(101n)).toBe("ok");
    expect(loadLastSeenLogSize()).toBe(101n);
  });

  it("handles sizes beyond Number.MAX_SAFE_INTEGER (bigint end to end)", () => {
    const big = 9_007_199_254_740_993n; // MAX_SAFE_INTEGER + 2
    expect(checkAndAdvance(big)).toBe("ok");
    expect(loadLastSeenLogSize()).toBe(big);
    expect(checkAndAdvance(big - 1n)).toBe("rollback");
    expect(loadLastSeenLogSize()).toBe(big);
  });

  it("treats a malformed stored value as never-seen (size 0)", () => {
    localStorage.setItem(
      "lakko.transparency.last-seen-root.v1",
      "not-a-number",
    );
    expect(loadLastSeenLogSize()).toBe(0n);
    // From a 0 baseline any size is accepted.
    expect(checkAndAdvance(3n)).toBe("ok");
    expect(loadLastSeenLogSize()).toBe(3n);
  });

  it("clearLastSeenRoot resets the high-water mark", () => {
    checkAndAdvance(50n);
    clearLastSeenRoot();
    expect(loadLastSeenLogSize()).toBe(0n);
  });
});
