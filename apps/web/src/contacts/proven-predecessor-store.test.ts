// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0066 section 3.5 — the contact store after a PROVEN rotation (a re-pin
 * the fail-closed gate authorised with a supersession proof): the replaced
 * key is a proven predecessor, the contact is NOT flagged, its row and its
 * one-to-one are untouched, and both keys name the same person. An unproven
 * change, and a confirmed reset, never leave a predecessor.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

class MemoryStorage {
  private map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.get(key) ?? null;
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

const OWNER = "b3duZXItYWNjb3VudC0wMDAwMA==";

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
  accountIdB64OfHex,
  accountIdB64OfKey,
  accountKeysOf,
  bindContactsAccount,
  confirmKeyReset,
  contactByAnyKey,
  loadAccepted,
  loadAccountKeys,
  loadFlaggedContacts,
  loadKeyAliases,
  peerContactByMemberPubkeys,
  readKeyReset,
  readKeyState,
  readPin,
  recordKeyReset,
  setPin,
  upsertAccepted,
  type AcceptedContact,
} from "./local-store";
import { bindingAccepted, buildPredecessorTable, conversationKeyOf, samePerson } from "./predecessor";

const ANNA_HEX = "0123456789abcdef0123456789abcdef";
const ANNA = accountIdB64OfHex(ANNA_HEX) as string;
const OLD = "b2xkLWtleS1vbGQta2V5LW9sZC1rZXktb2xkLWtleQ==";
const NEW = "bmV3LWtleS1uZXcta2V5LW5ldy1rZXktbmV3LWtleQ==";
const NEWER = "bmV3ZXIta2V5LW5ld2VyLWtleS1uZXdlci1rZXktbmU=";

function annaRow(): AcceptedContact {
  return {
    signaturePubkeyB64: OLD,
    displayName: "Anna",
    acceptedAtMs: 1,
    accountIdHex: ANNA_HEX,
    groupIdB64: "old-chat",
    x25519PubkeyB64: "x",
  };
}

describe("a proven rotation of a contact (the gate's re-pin)", () => {
  it("records the replaced key as a proven predecessor and flags nothing", () => {
    upsertAccepted(annaRow());
    setPin(ANNA, OLD, 0);
    expect(setPin(ANNA, NEW, 500, undefined, { proven: true })).toBe("set");
    const pin = readPin(ANNA);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(NEW);
    expect(pin?.provenPredecessorsB64).toEqual([OLD]);
    // Not a reset: no record, no state, nothing for the eviction arm.
    expect(readKeyReset(ANNA)).toBeUndefined();
    expect(readKeyState(ANNA)).toBe("normal");
    expect(loadFlaggedContacts()).toEqual([]);
  });

  it("leaves the contact row, its one-to-one and its keys exactly as they were", () => {
    upsertAccepted(annaRow());
    setPin(ANNA, OLD, 0);
    setPin(ANNA, NEW, 500, undefined, { proven: true });
    expect(loadAccepted()).toEqual([annaRow()]);
  });

  it("both keys name the same person: the leaves of the old key and anything signed by the new one", () => {
    upsertAccepted(annaRow());
    setPin(ANNA, OLD, 0);
    setPin(ANNA, NEW, 500, undefined, { proven: true });
    const aliases = loadKeyAliases();
    const accepted = loadAccepted();
    expect(contactByAnyKey(OLD, accepted, aliases)?.displayName).toBe("Anna");
    expect(contactByAnyKey(NEW, accepted, aliases)?.displayName).toBe("Anna");
    // The one-to-one peer is still Anna, whichever key the roster names.
    expect(peerContactByMemberPubkeys(["me", OLD], "me", accepted, aliases)?.displayName).toBe("Anna");
    expect(peerContactByMemberPubkeys(["me", NEW], "me", accepted, aliases)?.displayName).toBe("Anna");
    const keys = accountKeysOf(ANNA);
    expect(keys).toEqual({ currentKeyB64: NEW, provenPredecessorsB64: [OLD] });
    const table = buildPredecessorTable(loadAccountKeys());
    expect(samePerson(OLD, NEW, table)).toBe(true);
    // A device binding under the old key is still Anna's device.
    expect(bindingAccepted(OLD, keys as NonNullable<typeof keys>)).toBe(true);
    expect(conversationKeyOf(keys as NonNullable<typeof keys>)).toBe(OLD);
  });

  it("a second proven rotation keeps the chain", () => {
    setPin(ANNA, OLD, 0);
    setPin(ANNA, NEW, 500, undefined, { proven: true });
    setPin(ANNA, NEWER, 900, undefined, { proven: true });
    expect(accountKeysOf(ANNA)).toEqual({ currentKeyB64: NEWER, provenPredecessorsB64: [OLD, NEW] });
  });

  it("re-observing the same proven key changes nothing", () => {
    setPin(ANNA, OLD, 0);
    setPin(ANNA, NEW, 500, undefined, { proven: true });
    setPin(ANNA, NEW, 500, undefined, { proven: true });
    expect(accountKeysOf(ANNA)).toEqual({ currentKeyB64: NEW, provenPredecessorsB64: [OLD] });
  });
});

describe("an unproven change never gets predecessor treatment", () => {
  it("a re-pin that is not proven leaves no predecessor, and ends an earlier chain", () => {
    setPin(ANNA, OLD, 0);
    setPin(ANNA, NEW, 500, undefined, { proven: true });
    // Any other writer of a changed key (not the gate's proven branch).
    setPin(ANNA, NEWER, 900);
    expect(accountKeysOf(ANNA)).toEqual({ currentKeyB64: NEWER, provenPredecessorsB64: [] });
    const table = buildPredecessorTable(loadAccountKeys());
    expect(samePerson(OLD, NEWER, table)).toBe(false);
    expect(samePerson(NEW, NEWER, table)).toBe(false);
  });

  it("a reset is a flag, not a predecessor: the pin does not move and no key is proven", () => {
    setPin(ANNA, OLD, 0);
    expect(recordKeyReset(ANNA, NEW, 2, 1000)).toBe("recorded");
    expect(readKeyState(ANNA)).toBe("reset-unverified");
    expect(accountKeysOf(ANNA)).toEqual({ currentKeyB64: OLD, provenPredecessorsB64: [] });
  });

  it("a confirmed reset after a proven rotation ends every predecessor", () => {
    upsertAccepted(annaRow());
    setPin(ANNA, OLD, 0);
    setPin(ANNA, NEW, 500, undefined, { proven: true });
    recordKeyReset(ANNA, NEWER, 3, 2000);
    expect(confirmKeyReset(ANNA, 3000)).toBe("confirmed");
    expect(accountKeysOf(ANNA)).toEqual({ currentKeyB64: NEWER, provenPredecessorsB64: [] });
    const table = buildPredecessorTable(loadAccountKeys());
    expect(samePerson(OLD, NEWER, table)).toBe(false);
  });

  it("a junk predecessor list in storage reads as none", () => {
    setPin(ANNA, OLD, 0);
    setPin(ANNA, NEW, 500, undefined, { proven: true });
    const storage = (globalThis as unknown as { localStorage: Storage }).localStorage;
    for (let i = 0; i < storage.length; i += 1) {
      const k = storage.key(i) as string;
      const raw = storage.getItem(k) as string;
      if (!raw.includes("provenPredecessorsB64")) continue;
      storage.setItem(k, raw.replace(/"provenPredecessorsB64":\[[^\]]*\]/, '"provenPredecessorsB64":"junk"'));
    }
    bindContactsAccount(null);
    bindContactsAccount(OWNER);
    expect(accountKeysOf(ANNA)).toEqual({ currentKeyB64: NEW, provenPredecessorsB64: [] });
  });
});

describe("the account a key is known under (ADR-0066 section 3.5a rule 6)", () => {
  it("a pinned key and its proven predecessors name the account; a stranger's key names none", () => {
    setPin(ANNA, OLD, 0);
    setPin(ANNA, NEW, 500, undefined, { proven: true });
    expect(accountIdB64OfKey(NEW)).toBe(ANNA);
    expect(accountIdB64OfKey(OLD)).toBe(ANNA);
    expect(accountIdB64OfKey(NEWER)).toBeNull();
  });

  it("an accepted contact's row names the account when no pin does", () => {
    upsertAccepted(annaRow());
    expect(accountIdB64OfKey(OLD)).toBe(ANNA);
  });
});
