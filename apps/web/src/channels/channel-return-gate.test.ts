// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 sections 7 and 11 (walk of 2026-11-28): a peer sends channel
 * returns, profiles and successor conversations only to an account whose
 * key reset this device judged SEALED. Unsealed, refused or not yet judged:
 * nothing; an account that started over after an earlier sealed reset:
 * nothing either (`liveReturnTargets`, `loadMovableSupersededKeys`,
 * `acceptedResetOfKey`).
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  acceptReset,
  acceptedResetOfKey,
  accountIdB64OfHex,
  adoptStartedOver,
  loadStartedOverKeys,
  bindContactsAccount,
  loadMovableSupersededKeys,
  loadSupersededKeys,
  recordStartedOver,
  setPin,
} from "../contacts/local-store";
import { continuityFor } from "../reach/mailbox-write";
import { base64Decode } from "../auth/account-id";
import { liveReturnTargets } from "./channel-return-runtime";

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

const OWNER = "b3duZXItYWNjb3VudC0wMDAwMA==";
const ANNA = accountIdB64OfHex("0123456789abcdef0123456789abcdef") as string;
const K0 = "a2V5LTAta2V5LTAta2V5LTAta2V5LTAta2V5LTAtazA=";
const K1 = "a2V5LTEta2V5LTEta2V5LTEta2V5LTEta2V5LTEtazE=";
const K2 = "a2V5LTIta2V5LTIta2V5LTIta2V5LTIta2V5LTIta2s=";
const K3 = "a2V5LTMta2V5LTMta2V5LTMta2V5LTMta2V5LTMta2s=";
const SIGNED_AT = 1_700_000_000_000;
const NOW = SIGNED_AT + 60_000;

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
  bindContactsAccount(OWNER);
  setPin(ANNA, K0, 0, undefined, { keyGeneration: 1 });
});
afterEach(() => {
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
});

function sealed(): void {
  expect(
    acceptReset({
      accountIdB64: ANNA,
      heldKeyB64: K0,
      newKeyB64: K1,
      keyGeneration: 2,
      signedAtMs: SIGNED_AT,
      nowMs: SIGNED_AT + 1_000,
    }),
  ).toBe("accepted");
}

function unsealed(heldKeyB64: string, newKeyB64: string, keyGeneration: number): void {
  expect(
    recordStartedOver({
      accountIdB64: ANNA,
      heldKeyB64,
      newKeyB64,
      keyGeneration,
      signedAtMs: SIGNED_AT,
      nowMs: SIGNED_AT + 1_000,
    }),
  ).toBe("recorded");
}

describe("who a key reset sends anything to", () => {
  it("a SEALED reset: the retired key is a return target and the continuity key is usable", () => {
    sealed();
    expect([...liveReturnTargets(NOW)]).toEqual([[K0, ANNA]]);
    expect(loadMovableSupersededKeys().get(K0)).toBe(ANNA);
    expect(continuityFor(base64Decode(K1), NOW)).not.toBeNull();
  });

  it("an UNSEALED reset: nothing (no target, no movable key, no continuity key)", () => {
    unsealed(K0, K1, 2);
    expect(liveReturnTargets(NOW).size).toBe(0);
    expect(loadMovableSupersededKeys().size).toBe(0);
    expect(continuityFor(base64Decode(K1), NOW)).toBeNull();
    expect(continuityFor(base64Decode(K0), NOW)).toBeNull();
  });

  it("a refused reset, or one with no verdict yet, writes nothing: nothing is sent", () => {
    // The verdict code writes only on `sealed` (acceptReset) or `unsealed`
    // (recordStartedOver); a refusal or a missing verdict leaves the pin as
    // it was.
    expect(liveReturnTargets(NOW).size).toBe(0);
    expect(loadMovableSupersededKeys().size).toBe(0);
    expect(acceptedResetOfKey(K0)).toBeNull();
    expect(continuityFor(base64Decode(K0), NOW)).toBeNull();
  });

  it("an account that started over after an earlier SEALED reset: nothing from then on", () => {
    sealed();
    expect(liveReturnTargets(NOW).size).toBe(1);
    unsealed(K1, K2, 3);
    expect(liveReturnTargets(NOW).size).toBe(0);
    expect(loadMovableSupersededKeys().size).toBe(0);
    expect(acceptedResetOfKey(K1)).toBeNull();
    expect(continuityFor(base64Decode(K1), NOW)).toBeNull();
    expect(continuityFor(base64Decode(K2), NOW)).toBeNull();
    // The retired key of the sealed reset is still dropped on receipt.
    expect(loadSupersededKeys().get(K0)).toBe(ANNA);
  });

  it("sealed, then unsealed, then the person accepts the new account: nothing moves", () => {
    sealed();
    unsealed(K1, K2, 3);
    expect(adoptStartedOver(ANNA, K2, NOW)).toBe("adopted");
    // The relationship with the earlier selves ended: their keys are
    // left behind, none is retired toward the new account's key.
    expect([...loadStartedOverKeys().keys()].sort()).toEqual([K0, K1].sort());
    expect(liveReturnTargets(NOW).size).toBe(0);
    expect(loadMovableSupersededKeys().size).toBe(0);
    expect(loadSupersededKeys().size).toBe(0);
    for (const k of [K0, K1, K2]) {
      expect(acceptedResetOfKey(k)).toBeNull();
      expect(continuityFor(base64Decode(k), NOW)).toBeNull();
    }
  });

  it("... and a later SEALED reset of the new account moves only that reset's own keys", () => {
    sealed();
    unsealed(K1, K2, 3);
    adoptStartedOver(ANNA, K2, NOW);
    const later = SIGNED_AT + 120_000;
    expect(
      acceptReset({
        accountIdB64: ANNA,
        heldKeyB64: K2,
        newKeyB64: K3,
        keyGeneration: 4,
        signedAtMs: later,
        nowMs: later + 1_000,
      }),
    ).toBe("accepted");
    const at = later + 60_000;
    expect([...liveReturnTargets(at)]).toEqual([[K2, ANNA]]);
    expect([...loadMovableSupersededKeys()]).toEqual([[K2, ANNA]]);
    expect(acceptedResetOfKey(K3)).toMatchObject({ accountIdB64: ANNA, keyGeneration: 4, signedAtMs: later });
    expect(continuityFor(base64Decode(K3), at)).not.toBeNull();
    // The ended relationship's keys stay left behind, never retired.
    expect([...loadStartedOverKeys().keys()].sort()).toEqual([K0, K1].sort());
  });

  it("past the continuity window a sealed reset sends nothing either", () => {
    sealed();
    expect(liveReturnTargets(SIGNED_AT + 365 * 24 * 60 * 60 * 1000).size).toBe(0);
  });
});
