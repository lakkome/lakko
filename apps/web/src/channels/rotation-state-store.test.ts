// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Unit tests for the per-channel `ChannelRotationState` persistence
 * (`rotation-state-store.ts`). Runs under the default vitest `node`
 * environment (no `localStorage`); a minimal in-memory shim is
 * installed per test so the store's `typeof localStorage` guard sees
 * a real backend.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

/** Minimal synchronous in-memory `localStorage` Web Storage shim. */
class MemoryStorage {
  private map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.has(key) ? (this.map.get(key) as string) : null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, String(value));
  }
  removeItem(key: string): void {
    this.map.delete(key);
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

/** The signed-in account most tests run as. */
const OWNER = "b3duZXItYWNjb3VudC0wMDAwMA==";
const OTHER = "b3RoZXItYWNjb3VudC0wMDAwMA==";

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
  bindContactsAccount(OWNER);
});

afterEach(() => {
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
});

import {
  clearRotationStatesForAccount,
  loadRotationState,
  saveRotationState,
} from "./rotation-state-store";
import { bindContactsAccount } from "../contacts/local-store";

const CH = "00112233445566778899aabbccddeeff";

describe("rotation-state-store", () => {
  it("returns null before anything is persisted", () => {
    expect(loadRotationState(CH)).toBeNull();
  });

  it("round-trips a state, preserving the bigint lastRotationMs", () => {
    const state = { lastRotationMs: 1_700_000_000_000n, postsSinceRotation: 7 };
    saveRotationState(CH, state);
    const loaded = loadRotationState(CH);
    expect(loaded).not.toBeNull();
    expect(loaded!.lastRotationMs).toBe(1_700_000_000_000n);
    expect(typeof loaded!.lastRotationMs).toBe("bigint");
    expect(loaded!.postsSinceRotation).toBe(7);
  });

  it("round-trips the genesis {0,0} state", () => {
    saveRotationState(CH, { lastRotationMs: 0n, postsSinceRotation: 0 });
    const loaded = loadRotationState(CH);
    expect(loaded).toEqual({ lastRotationMs: 0n, postsSinceRotation: 0 });
  });

  it("keys per channel — one channel's state does not leak into another", () => {
    saveRotationState(CH, { lastRotationMs: 42n, postsSinceRotation: 1 });
    const other = "ffffffffffffffffffffffffffffffff";
    expect(loadRotationState(other)).toBeNull();
    expect(loadRotationState(CH)!.lastRotationMs).toBe(42n);
  });

  it("treats a corrupt entry as absent rather than throwing", () => {
    localStorage.setItem(
      `lakko.channels.rotation-state.v2.${OWNER}.${CH}`,
      "{ not valid json",
    );
    expect(loadRotationState(CH)).toBeNull();
  });

  it("rejects a negative post count as corrupt (returns null)", () => {
    localStorage.setItem(
      `lakko.channels.rotation-state.v2.${OWNER}.${CH}`,
      JSON.stringify({ lastRotationMs: "1", postsSinceRotation: -3 }),
    );
    expect(loadRotationState(CH)).toBeNull();
  });

  it("two accounts on one browser never see each other's schedule", () => {
    saveRotationState(CH, { lastRotationMs: 42n, postsSinceRotation: 1 });
    bindContactsAccount(OTHER);
    expect(loadRotationState(CH)).toBeNull();
    saveRotationState(CH, { lastRotationMs: 7n, postsSinceRotation: 2 });
    bindContactsAccount(OWNER);
    expect(loadRotationState(CH)!.lastRotationMs).toBe(42n);
    expect(loadRotationState(CH, OTHER)!.lastRotationMs).toBe(7n);
  });

  it("with no account a read is null and a write is a no-op", () => {
    bindContactsAccount(null);
    saveRotationState(CH, { lastRotationMs: 42n, postsSinceRotation: 1 });
    expect(localStorage.length).toBe(0);
    expect(loadRotationState(CH)).toBeNull();
  });

  it("the retired device-wide row is not read", () => {
    localStorage.setItem(
      "lakko.channels.rotation-state.v1." + CH,
      JSON.stringify({ lastRotationMs: "5", postsSinceRotation: 1 }),
    );
    expect(loadRotationState(CH)).toBeNull();
  });

  it("Forget device drops the account's rows and the retired ones, never another account's", () => {
    localStorage.setItem("lakko.channels.rotation-state.v1." + CH, "{}");
    saveRotationState(CH, { lastRotationMs: 42n, postsSinceRotation: 1 });
    saveRotationState(CH, { lastRotationMs: 7n, postsSinceRotation: 2 }, OTHER);

    clearRotationStatesForAccount(OWNER);

    expect(loadRotationState(CH)).toBeNull();
    expect(loadRotationState(CH, OTHER)!.lastRotationMs).toBe(7n);
    expect(localStorage.getItem("lakko.channels.rotation-state.v1." + CH)).toBeNull();
  });
});
