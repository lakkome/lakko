// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0073 R.4 - the continuity key's window and marker (`continuity.ts`),
 *  and that both sides derive the same key from the wasm core. */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  CONTINUITY_KEY_TTL_MS,
  clearContinuityMarker,
  continuityExpiryMs,
  continuityKeyUsable,
  continuityKeyWanted,
  readContinuityMarker,
  writeContinuityMarker,
} from "./continuity";

class MemoryStorage {
  readonly map = new Map<string, string>();
  getItem(k: string): string | null {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, v);
  }
  removeItem(k: string): void {
    this.map.delete(k);
  }
}

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

describe("the continuity window (D2: 30 days from the reset's signed_at)", () => {
  it("is the DS journal retention", () => {
    expect(CONTINUITY_KEY_TTL_MS).toBe(30 * 24 * 60 * 60 * 1000);
    expect(continuityExpiryMs(1000)).toBe(1000 + CONTINUITY_KEY_TTL_MS);
  });
  it("the resetting device wants the key registered until the expiry, with it", () => {
    const marker = { keyGeneration: 3, signedAtMs: 1000 };
    expect(continuityKeyWanted(marker, 2000)).toEqual({
      keyGeneration: 3,
      expiresAtMs: 1000 + CONTINUITY_KEY_TTL_MS,
    });
    expect(continuityKeyWanted(marker, 1000 + CONTINUITY_KEY_TTL_MS)).toBeNull();
    expect(continuityKeyWanted(null, 2000)).toBeNull();
  });
  it("a peer uses it inside the same window only", () => {
    expect(continuityKeyUsable(1000, 1000 + CONTINUITY_KEY_TTL_MS - 1)).toBe(true);
    expect(continuityKeyUsable(1000, 1000 + CONTINUITY_KEY_TTL_MS)).toBe(false);
  });
});

describe("the marker", () => {
  it("round-trips, and is removed with the account's data", () => {
    const storage = new MemoryStorage();
    writeContinuityMarker(storage, "acct", { keyGeneration: 2, signedAtMs: 7 });
    expect(readContinuityMarker(storage, "acct")).toEqual({ keyGeneration: 2, signedAtMs: 7 });
    expect(readContinuityMarker(storage, "other")).toBeNull();
    clearContinuityMarker(storage, "acct");
    expect(readContinuityMarker(storage, "acct")).toBeNull();
  });
  it("reads junk as none", () => {
    const storage = new MemoryStorage();
    storage.setItem("lakko:reset-continuity:acct", "{");
    expect(readContinuityMarker(storage, "acct")).toBeNull();
    storage.setItem("lakko:reset-continuity:acct", JSON.stringify({ keyGeneration: 0, signedAtMs: 1 }));
    expect(readContinuityMarker(storage, "acct")).toBeNull();
    expect(readContinuityMarker(undefined, "acct")).toBeNull();
  });
});

describe("both sides derive one key (the wasm core)", () => {
  const ACCOUNT = new Uint8Array(16).fill(5);
  it("the seed is a function of the account id and the generation only", () => {
    const a = wasm.continuityReachSeed(ACCOUNT, 2n);
    const b = wasm.continuityReachSeed(ACCOUNT, 2n);
    expect(Array.from(a)).toEqual(Array.from(b));
    expect(a.length).toBe(32);
    expect(Array.from(wasm.continuityReachSeed(ACCOUNT, 3n))).not.toEqual(Array.from(a));
    expect(Array.from(wasm.continuityReachSeed(new Uint8Array(16).fill(6), 2n))).not.toEqual(Array.from(a));
  });
  it("the public key the owner registers is the one the peer's signer proves under", () => {
    const seed = wasm.continuityReachSeed(ACCOUNT, 2n);
    const registered = wasm.reachVerifyingKey(seed);
    const signer = wasm.DsProofSigner.forReach(wasm.continuityReachSeed(ACCOUNT, 2n));
    try {
      expect(Array.from(signer.verifyingKey())).toEqual(Array.from(registered));
    } finally {
      signer.free();
    }
  });
});
