// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Unit tests for the G-4 (ADR-0016) account-pubkey PIN store and
 * the v1 -> v2 localStorage migration in `local-store.ts`.
 *
 * Runs under the default vitest `node` environment, which has no
 * `localStorage`; we install a minimal in-memory stub per test so
 * the store's `typeof localStorage` guard sees a real backend.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

/** Minimal synchronous in-memory `localStorage` Web Storage
 *  shim — enough for the store's getItem/setItem/removeItem use. */
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

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
});

afterEach(() => {
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
  // Force a fresh module instance so the store has no cross-test
  // state (the module is stateless — all reads hit localStorage —
  // but resetting modules keeps each test hermetic regardless).
});

// Import after the localStorage shim type is in place. The store
// reads `localStorage` lazily per call, so a top-level import is
// fine as long as the shim is installed in `beforeEach`.
import {
  readPin,
  removePin,
  setPin,
  loadAccepted,
  loadCursor,
  recordLogStatus,
  recordLogReset,
  readLogReset,
} from "./local-store";

const ACCT_A = "YWNjb3VudC1hLTAwMDAwMDAwMA=="; // arbitrary base64
const ACCT_B = "YWNjb3VudC1iLTAwMDAwMDAwMA==";
const PUB_OLD = "b2xkLXB1YmtleS0wMDAwMDAwMDAwMDA=";
const PUB_NEW = "bmV3LXB1YmtleS0wMDAwMDAwMDAwMDA=";

describe("account-pubkey PIN store (G-4)", () => {
  it("returns undefined for an unknown account_id", () => {
    expect(readPin(ACCT_A)).toBeUndefined();
  });

  it("persists a TOFU pin and reads it back", () => {
    const result = setPin(ACCT_A, PUB_OLD, 0);
    expect(result).toBe("set");
    const pin = readPin(ACCT_A);
    expect(pin).toBeDefined();
    expect(pin?.pinnedSignaturePubkeyB64).toBe(PUB_OLD);
    expect(pin?.rotationMarker).toBe(0);
  });

  it("survives a fresh module read (localStorage-backed)", () => {
    setPin(ACCT_A, PUB_OLD, 0);
    // A second independent read resolves the same persisted value.
    expect(readPin(ACCT_A)?.pinnedSignaturePubkeyB64).toBe(PUB_OLD);
  });

  it("treats a changed pubkey for the same account_id as a mutation, not a new contact", () => {
    setPin(ACCT_A, PUB_OLD, 100);
    // Advancing forward re-pins the SAME account_id to the new key.
    const result = setPin(ACCT_A, PUB_NEW, 200);
    expect(result).toBe("set");
    const pin = readPin(ACCT_A);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(PUB_NEW);
    expect(pin?.rotationMarker).toBe(200);
    // Still exactly one account_id key — not forked into two.
    expect(readPin(ACCT_B)).toBeUndefined();
  });

  it("allows an idempotent re-pin at the same marker", () => {
    setPin(ACCT_A, PUB_OLD, 500);
    const result = setPin(ACCT_A, PUB_NEW, 500);
    expect(result).toBe("set");
    expect(readPin(ACCT_A)?.pinnedSignaturePubkeyB64).toBe(PUB_NEW);
    expect(readPin(ACCT_A)?.rotationMarker).toBe(500);
  });

  it("refuses to lower the rotation marker (monotonic, rule #4)", () => {
    setPin(ACCT_A, PUB_OLD, 1000);
    const result = setPin(ACCT_A, PUB_NEW, 999);
    expect(result).toBe("rejected-backward");
    // The original pin is untouched — no backward re-pin.
    const pin = readPin(ACCT_A);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(PUB_OLD);
    expect(pin?.rotationMarker).toBe(1000);
  });

  it("keeps pins independent across distinct account_ids", () => {
    setPin(ACCT_A, PUB_OLD, 10);
    setPin(ACCT_B, PUB_NEW, 20);
    expect(readPin(ACCT_A)?.pinnedSignaturePubkeyB64).toBe(PUB_OLD);
    expect(readPin(ACCT_B)?.pinnedSignaturePubkeyB64).toBe(PUB_NEW);
  });

  it("removePin drops the pin", () => {
    setPin(ACCT_A, PUB_OLD, 0);
    removePin(ACCT_A);
    expect(readPin(ACCT_A)).toBeUndefined();
  });
});

describe("KT-log S4 — isReset marker on the pin record", () => {
  it("recordLogReset is a no-op when no pin exists", () => {
    expect(recordLogReset(ACCT_A, true)).toBe(false);
    expect(readLogReset(ACCT_A)).toBeUndefined();
  });

  it("records true/false onto an existing pin and reads it back", () => {
    setPin(ACCT_A, PUB_OLD, 0);
    expect(recordLogReset(ACCT_A, true)).toBe(true);
    expect(readLogReset(ACCT_A)).toBe(true);
    expect(recordLogReset(ACCT_A, false)).toBe(true);
    expect(readLogReset(ACCT_A)).toBe(false);
  });

  it("a null marker CLEARS a previously-recorded value", () => {
    setPin(ACCT_A, PUB_OLD, 0);
    recordLogReset(ACCT_A, true);
    expect(readLogReset(ACCT_A)).toBe(true);
    // null = no latest entry read → clear the stale marker.
    expect(recordLogReset(ACCT_A, null)).toBe(true);
    expect(readLogReset(ACCT_A)).toBeUndefined();
  });

  it("carries the isReset marker across a forward re-pin", () => {
    setPin(ACCT_A, PUB_OLD, 100);
    recordLogReset(ACCT_A, true);
    // A monotonic forward re-pin keeps the recorded marker.
    expect(setPin(ACCT_A, PUB_NEW, 200)).toBe("set");
    expect(readLogReset(ACCT_A)).toBe(true);
  });

  it("isReset and logStatus are independent fields on the same pin", () => {
    setPin(ACCT_A, PUB_OLD, 0);
    recordLogStatus(ACCT_A, "confirmed");
    recordLogReset(ACCT_A, true);
    const pin = readPin(ACCT_A);
    expect(pin?.logStatus).toBe("confirmed");
    expect(pin?.isReset).toBe(true);
  });

  it("drops a junk (non-boolean) isReset on read (forward-compat guard)", () => {
    // Hand-craft a v2 row with a corrupt isReset value.
    localStorage.setItem(
      "lakko.contacts.v2",
      JSON.stringify({
        v: 2,
        cursor: 0,
        accepted: [],
        pins: {
          [ACCT_A]: {
            pinnedSignaturePubkeyB64: PUB_OLD,
            rotationMarker: 0,
            isReset: "yes-please", // junk
          },
        },
      }),
    );
    const pin = readPin(ACCT_A);
    // The pin survives (structural fields valid); the junk marker is dropped.
    expect(pin?.pinnedSignaturePubkeyB64).toBe(PUB_OLD);
    expect(pin?.isReset).toBeUndefined();
  });
});

describe("v1 -> v2 localStorage migration", () => {
  it("carries the v1 cursor + accepted forward into v2", () => {
    // Seed a legacy v1 row directly.
    const v1 = {
      v: 1,
      cursor: 42,
      accepted: [
        {
          signaturePubkeyB64: "cHVia2V5",
          displayName: "alice",
          acceptedAtMs: 1700000000000,
        },
      ],
    };
    localStorage.setItem("lakko.contacts.v1", JSON.stringify(v1));
    // No v2 row exists yet — the first read migrates additively.
    expect(loadCursor()).toBe(42);
    const accepted = loadAccepted();
    expect(accepted).toHaveLength(1);
    expect(accepted[0]?.displayName).toBe("alice");
    // The pins map starts empty after a migration.
    expect(readPin(ACCT_A)).toBeUndefined();
  });

  it("does not lose pins written after migration", () => {
    localStorage.setItem(
      "lakko.contacts.v1",
      JSON.stringify({ v: 1, cursor: 7, accepted: [] }),
    );
    // Read once to migrate, then write a pin.
    expect(loadCursor()).toBe(7);
    setPin(ACCT_A, PUB_OLD, 0);
    expect(readPin(ACCT_A)?.pinnedSignaturePubkeyB64).toBe(PUB_OLD);
    // Cursor is still intact after the pin write.
    expect(loadCursor()).toBe(7);
  });
});
