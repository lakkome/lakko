// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 phase 2 (UX 16.1 / 16.2 / 16.8) — the contact store's reset
 * state: recording a reset beside the pin without advancing it, the
 * confirmation that re-pins and re-keys the row, the aliases that keep the
 * person named across the key change, and what removing the contact does.
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

import { adoptOneToOnePeer } from "./adopt-one-to-one";
import {
  accountIdB64OfHex,
  bindContactsAccount,
  confirmKeyReset,
  contactByAnyKey,
  keyResetOfContact,
  loadAccepted,
  loadFlaggedContacts,
  loadKeyAliases,
  peerContactByMemberPubkeys,
  readKeyReset,
  readKeyState,
  readPin,
  recordKeyReset,
  removeAccepted,
  setPin,
  subscribeAcceptedChanged,
  upsertAccepted,
  type AcceptedContact,
} from "./local-store";

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

describe("accountIdB64OfHex", () => {
  it("bridges the row's hex id to the pin map's base64 key, refusing junk", () => {
    expect(ANNA).toBe("ASNFZ4mrze8BI0VniavN7w==");
    expect(accountIdB64OfHex(undefined)).toBeNull();
    expect(accountIdB64OfHex("zz")).toBeNull();
  });
});

describe("recordKeyReset — the flagged contact (16.1)", () => {
  it("records beside the pin WITHOUT advancing it (invariant #6)", () => {
    setPin(ANNA, OLD, 0);
    expect(recordKeyReset(ANNA, NEW, 2, 1000)).toBe("recorded");
    expect(readPin(ANNA)?.pinnedSignaturePubkeyB64).toBe(OLD);
    expect(readKeyReset(ANNA)).toEqual({
      previousKeyB64: OLD,
      pendingKeyB64: NEW,
      noticedAtMs: 1000,
      keyGeneration: 2,
    });
    expect(readKeyState(ANNA)).toBe("reset-unverified");
  });

  it("is idempotent on the same key, and keeps the ORIGINAL previousKey on a second reset", () => {
    setPin(ANNA, OLD, 0);
    recordKeyReset(ANNA, NEW, 2, 1000);
    expect(recordKeyReset(ANNA, NEW, 2, 2000)).toBe("already");
    expect(readKeyReset(ANNA)?.noticedAtMs).toBe(1000);
    expect(recordKeyReset(ANNA, NEWER, 3, 3000)).toBe("recorded");
    expect(readKeyReset(ANNA)).toMatchObject({ previousKeyB64: OLD, pendingKeyB64: NEWER, keyGeneration: 3 });
  });

  it("needs a pin, refuses the pin itself, and refuses a generation already confirmed", () => {
    expect(recordKeyReset(ANNA, NEW, 2, 1)).toBe("no-pin");
    setPin(ANNA, OLD, 0);
    expect(recordKeyReset(ANNA, OLD, 2, 1)).toBe("not-a-change");
    recordKeyReset(ANNA, NEW, 2, 1);
    confirmKeyReset(ANNA, 10);
    expect(recordKeyReset(ANNA, NEW, 2, 1)).toBe("not-a-change");
    expect(recordKeyReset(ANNA, OLD, 1, 1)).toBe("stale");
    expect(readKeyState(ANNA)).toBe("normal");
  });

  it("tells the accepted-list listeners, so every screen re-reads", () => {
    setPin(ANNA, OLD, 0);
    let told = 0;
    const unsubscribe = subscribeAcceptedChanged(() => {
      told += 1;
    });
    recordKeyReset(ANNA, NEW, 2, 1);
    expect(told).toBe(1);
    unsubscribe();
  });

  it("without an account bound nothing is written", () => {
    bindContactsAccount(null);
    expect(recordKeyReset(ANNA, NEW, 2, 1)).toBe("no-account");
  });
});

describe("the flagged set and the row", () => {
  it("lists the flagged contacts with their records, by the row's account id", () => {
    upsertAccepted(annaRow());
    setPin(ANNA, OLD, 0);
    expect(loadFlaggedContacts()).toEqual([]);
    recordKeyReset(ANNA, NEW, 2, 1);
    const flagged = loadFlaggedContacts();
    expect(flagged).toHaveLength(1);
    expect(flagged[0]?.accountIdB64).toBe(ANNA);
    expect(flagged[0]?.contact.signaturePubkeyB64).toBe(OLD);
    expect(keyResetOfContact(annaRow())?.pendingKeyB64).toBe(NEW);
  });

  it("a row with no account id is never flagged", () => {
    const row = annaRow();
    delete row.accountIdHex;
    upsertAccepted(row);
    setPin(ANNA, OLD, 0);
    recordKeyReset(ANNA, NEW, 2, 1);
    expect(loadFlaggedContacts()).toEqual([]);
    expect(keyResetOfContact(row)).toBeUndefined();
  });
});

describe("confirmKeyReset — the person compared the new number (16.2)", () => {
  beforeEach(() => {
    upsertAccepted(annaRow());
    setPin(ANNA, OLD, 0);
    recordKeyReset(ANNA, NEW, 2, 1);
  });

  it("re-pins to the new key with a marker that is never backwards, records the generation", () => {
    expect(confirmKeyReset(ANNA, 1_700_000_000)).toBe("confirmed");
    const pin = readPin(ANNA);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(NEW);
    expect(pin?.rotationMarker).toBe(1_700_000_000);
    expect(pin?.confirmedKeyGeneration).toBe(2);
    expect(pin?.keyReset).toBeUndefined();
    expect(readKeyState(ANNA)).toBe("normal");
  });

  it("keeps the old key for the eviction bookkeeping", () => {
    confirmKeyReset(ANNA, 10);
    expect(readPin(ANNA)?.previousKeysB64).toEqual([OLD]);
  });

  it("a held marker larger than the clock is kept (equal is accepted by setPin, never refused)", () => {
    setPin(ANNA, OLD, 5_000_000_000);
    recordKeyReset(ANNA, NEW, 2, 1);
    expect(confirmKeyReset(ANNA, 10)).toBe("confirmed");
    expect(readPin(ANNA)?.rotationMarker).toBe(5_000_000_000);
    expect(readPin(ANNA)?.pinnedSignaturePubkeyB64).toBe(NEW);
  });

  it("re-keys the contact ROW to the new key: rename, account id stay; the old chat link goes", () => {
    confirmKeyReset(ANNA, 10);
    const rows = loadAccepted();
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({
      signaturePubkeyB64: NEW,
      displayName: "Anna",
      accountIdHex: ANNA_HEX,
      previousSignaturePubkeysB64: [OLD],
    });
    expect(rows[0]?.groupIdB64).toBeUndefined();
    expect(rows[0]?.x25519PubkeyB64).toBeUndefined();
  });

  it("refuses when nothing is flagged", () => {
    confirmKeyReset(ANNA, 10);
    expect(confirmKeyReset(ANNA, 10)).toBe("not-flagged");
  });

  it("a later reset of the same account is recorded again, never refused as backwards", () => {
    confirmKeyReset(ANNA, 10);
    expect(recordKeyReset(ANNA, NEWER, 3, 2)).toBe("recorded");
    expect(readKeyReset(ANNA)?.previousKeyB64).toBe(NEW);
    expect(confirmKeyReset(ANNA, 11)).toBe("confirmed");
    expect(readPin(ANNA)?.previousKeysB64).toEqual([OLD, NEW]);
  });
});

describe("the aliases — one person before and after the key change", () => {
  it("the pending key names the row while flagged; the retired key after confirmation", () => {
    upsertAccepted(annaRow());
    setPin(ANNA, OLD, 0);
    recordKeyReset(ANNA, NEW, 2, 1);
    expect(loadKeyAliases()).toEqual(new Map([[NEW, OLD]]));
    expect(contactByAnyKey(NEW, loadAccepted(), loadKeyAliases())?.displayName).toBe("Anna");
    expect(peerContactByMemberPubkeys([NEW], "me", loadAccepted(), loadKeyAliases())?.displayName).toBe("Anna");
    confirmKeyReset(ANNA, 10);
    expect(loadKeyAliases()).toEqual(new Map([[OLD, NEW]]));
    expect(peerContactByMemberPubkeys([OLD], "me", loadAccepted(), loadKeyAliases())?.signaturePubkeyB64).toBe(NEW);
    expect(peerContactByMemberPubkeys([OLD], "me", loadAccepted())).toBeUndefined();
  });

  it("the one-to-one adoption rule never files a second contact for the retired key", () => {
    upsertAccepted(annaRow());
    setPin(ANNA, OLD, 0);
    recordKeyReset(ANNA, NEW, 2, 1);
    confirmKeyReset(ANNA, 10);
    expect(
      adoptOneToOnePeer(loadAccepted(), { signatureKey: OLD, groupIdB64: "old-chat", nowMs: 5 }),
    ).toBeNull();
    // The NEW chat re-links the row.
    const relinked = adoptOneToOnePeer(loadAccepted(), { signatureKey: NEW, groupIdB64: "new-chat", nowMs: 5 });
    expect(relinked?.[0]?.groupIdB64).toBe("new-chat");
  });
});

describe("removeAccepted clears the state with the contact (16.1)", () => {
  it("drops the reset record but keeps the pin (this device's trust decision)", () => {
    upsertAccepted(annaRow());
    setPin(ANNA, OLD, 0);
    recordKeyReset(ANNA, NEW, 2, 1);
    removeAccepted(OLD);
    expect(readKeyReset(ANNA)).toBeUndefined();
    expect(readPin(ANNA)?.pinnedSignaturePubkeyB64).toBe(OLD);
    expect(loadFlaggedContacts()).toEqual([]);
  });
});
