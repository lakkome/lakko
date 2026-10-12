// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 R.2 "The re-pin" - what an accepted reset writes into the
 * contacts store (`local-store.ts` `acceptReset`), and what it leaves out:
 * no state on the contact, nothing that reads as "reset".
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  acceptReset,
  acceptedGenerationOf,
  accountIdB64OfHex,
  bindContactsAccount,
  contactByAnyKey,
  loadAccepted,
  loadKeyAliases,
  loadSupersededKeys,
  readPin,
  setPin,
  upsertAccepted,
} from "./local-store";

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
const ANNA_HEX = "0123456789abcdef0123456789abcdef";
const ANNA = accountIdB64OfHex(ANNA_HEX) as string;
const OLD = "b2xkLWtleQ==";
const NEW = "bmV3LWtleQ==";
const NEWER = "bmV3ZXIta2V5";

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
  bindContactsAccount(OWNER);
});
afterEach(() => {
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
});

function annaRow(): Parameters<typeof upsertAccepted>[0] {
  return {
    signaturePubkeyB64: OLD,
    displayName: "Anna (work)",
    acceptedAtMs: 1,
    accountIdHex: ANNA_HEX,
    groupIdB64: "the-chat",
    x25519PubkeyB64: "old-x",
  };
}

function accept(newKeyB64: string, heldKeyB64: string, keyGeneration: number, nowMs = 50_000) {
  return acceptReset({
    accountIdB64: ANNA,
    heldKeyB64,
    newKeyB64,
    newX25519B64: "new-x",
    keyGeneration,
    signedAtMs: 40_000,
    nowMs,
  });
}

describe("acceptReset", () => {
  it("re-pins to the new key, raises the generation, and keeps the marker monotone", () => {
    setPin(ANNA, OLD, 7, undefined, { keyGeneration: 1 });
    expect(acceptedGenerationOf(ANNA)).toBe(1);
    expect(accept(NEW, OLD, 2)).toBe("accepted");
    const pin = readPin(ANNA);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(NEW);
    expect(pin?.acceptedKeyGeneration).toBe(2);
    expect(pin?.rotationMarker).toBe(50);
    expect(pin?.previousKeysB64).toEqual([OLD]);
    expect(pin?.resetSignedAtMs).toBe(40_000);
  });

  it("never lowers a marker that is ahead of the clock", () => {
    setPin(ANNA, OLD, 9_000_000, undefined, { keyGeneration: 1 });
    accept(NEW, OLD, 2);
    expect(readPin(ANNA)?.rotationMarker).toBe(9_000_000);
  });

  it("re-keys the contact row keeping rename, account id and chat link", () => {
    upsertAccepted(annaRow());
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 1 });
    accept(NEW, OLD, 2);
    const rows = loadAccepted();
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({
      signaturePubkeyB64: NEW,
      displayName: "Anna (work)",
      accountIdHex: ANNA_HEX,
      groupIdB64: "the-chat",
      x25519PubkeyB64: "new-x",
      previousSignaturePubkeysB64: [OLD],
    });
  });

  it("keeps the old key as a display alias only", () => {
    upsertAccepted(annaRow());
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 1 });
    accept(NEW, OLD, 2);
    const aliases = loadKeyAliases();
    expect(aliases.get(OLD)).toBe(NEW);
    expect(contactByAnyKey(OLD, loadAccepted(), aliases)?.signaturePubkeyB64).toBe(NEW);
    // ...and the old key is superseded: rows and claims from it are dropped.
    expect(loadSupersededKeys().get(OLD)).toBe(ANNA);
    expect(loadSupersededKeys().has(NEW)).toBe(false);
  });

  it("refuses a generation that is not above the accepted one", () => {
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 3 });
    expect(accept(NEW, OLD, 3)).toBe("stale");
    expect(accept(NEW, OLD, 2)).toBe("stale");
    expect(readPin(ANNA)?.pinnedSignaturePubkeyB64).toBe(OLD);
  });

  it("is idempotent for the same key and generation", () => {
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 1 });
    expect(accept(NEW, OLD, 2)).toBe("accepted");
    expect(accept(NEW, OLD, 2)).toBe("already");
  });

  it("a second reset accumulates aliases and superseded keys (R.8)", () => {
    upsertAccepted(annaRow());
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 1 });
    accept(NEW, OLD, 2);
    expect(accept(NEWER, NEW, 3)).toBe("accepted");
    expect(readPin(ANNA)?.previousKeysB64).toEqual([OLD, NEW]);
    expect([...loadSupersededKeys().keys()].sort()).toEqual([NEW, OLD].sort());
    expect(loadAccepted()[0]?.signaturePubkeyB64).toBe(NEWER);
    expect(loadKeyAliases().get(OLD)).toBe(NEWER);
    expect(loadKeyAliases().get(NEW)).toBe(NEWER);
  });

  it("pins an account this device never pinned, from the roster's key", () => {
    expect(readPin(ANNA)).toBeUndefined();
    expect(accept(NEW, OLD, 2)).toBe("accepted");
    expect(readPin(ANNA)?.pinnedSignaturePubkeyB64).toBe(NEW);
    expect(loadSupersededKeys().get(OLD)).toBe(ANNA);
  });

  it("folds a second row already under the new key into the person's row", () => {
    upsertAccepted(annaRow());
    upsertAccepted({ signaturePubkeyB64: NEW, displayName: "", acceptedAtMs: 2 });
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 1 });
    accept(NEW, OLD, 2);
    const rows = loadAccepted();
    expect(rows).toHaveLength(1);
    expect(rows[0]?.displayName).toBe("Anna (work)");
  });

  it("leaves no state on the pin that reads as a reset in progress", () => {
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 1 });
    accept(NEW, OLD, 2);
    const pin = readPin(ANNA) as unknown as Record<string, unknown>;
    expect(pin["keyReset"]).toBeUndefined();
    expect(pin["keyState"]).toBeUndefined();
  });
});

describe("the accepted-generation baseline (setPin)", () => {
  it("is the bundle's generation at the first pin and is not replaced later", () => {
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 4 });
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 9 });
    expect(acceptedGenerationOf(ANNA)).toBe(4);
  });

  it("is 0 for a pin that never recorded one", () => {
    setPin(ANNA, OLD, 0);
    expect(acceptedGenerationOf(ANNA)).toBe(0);
  });
});
