// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 section 7 — a publisher whose request carried no account id (a
 * person with no display name has no profile, ADR-0048) is reached through
 * the owner's contact store, so the read key, the earlier seeds and the
 * admission seeds go to it. Seen live 2026-11-13 (base scenario 28): an
 * owner on web added an iPhone with no name as a publisher and sent it
 * nothing.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { accountIdB64OfHex, accountIdB64OfKey, bindContactsAccount, upsertAccepted } from "../contacts/local-store";
import type { PublisherEntry } from "./channel-access-record";
import {
  readKeyRecipients,
  resolvePublisherAccountIdHex,
  withResolvedAccountIds,
} from "./publisher-app-messages";

class MemoryStorage {
  private readonly map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, value);
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
const PUB_KEY = "cHViLWtleS1wdWIta2V5LXB1Yi1rZXktcHViLWtleS0=";
const STRANGER_KEY = "c3RyLWtleS1zdHIta2V5LXN0ci1rZXktc3RyLWtleS0=";
const PUB_ID_HEX = "00112233445566778899aabbccddeeff";
const CLAIMED_HEX = "ffeeddccbbaa99887766554433221100";

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage = new MemoryStorage() as unknown as Storage;
  bindContactsAccount(OWNER);
  upsertAccepted({
    signaturePubkeyB64: PUB_KEY,
    displayName: "",
    acceptedAtMs: 1,
    accountIdHex: PUB_ID_HEX,
  });
});
afterEach(() => {
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
});

function entry(accountKeyB64: string, accountIdHex: string): PublisherEntry {
  return { accountKeyB64, copublisherKeyB64: "", accountIdHex, sentVersions: [] };
}

describe("a publisher whose request carried no account id", () => {
  it("is resolved through the owner's contact row and is then owed the read key", () => {
    const entries = [entry(PUB_KEY, "")];
    // Before: nobody to send to.
    expect(readKeyRecipients(entries)).toEqual([]);
    const filled = withResolvedAccountIds(entries, (k) => accountIdB64OfKey(k));
    expect(filled[0]?.accountIdHex).toBe(PUB_ID_HEX);
    expect(readKeyRecipients(filled).map((p) => p.accountKeyB64)).toEqual([PUB_KEY]);
  });

  it("an approve with no carried id records the contact's id", () => {
    expect(resolvePublisherAccountIdHex("", PUB_KEY, (k) => accountIdB64OfKey(k))).toBe(PUB_ID_HEX);
  });

  it("keeps an id the request carried", () => {
    expect(resolvePublisherAccountIdHex(CLAIMED_HEX, PUB_KEY, (k) => accountIdB64OfKey(k))).toBe(CLAIMED_HEX);
  });

  it("stays unknown for someone who is not a contact, and changes nothing", () => {
    const entries = [entry(STRANGER_KEY, "")];
    expect(withResolvedAccountIds(entries, (k) => accountIdB64OfKey(k))).toBe(entries);
    expect(resolvePublisherAccountIdHex("", STRANGER_KEY, (k) => accountIdB64OfKey(k))).toBe("");
  });

  it("refuses a malformed contact id", () => {
    expect(resolvePublisherAccountIdHex("", PUB_KEY, () => "not base64!")).toBe("");
    expect(resolvePublisherAccountIdHex("", PUB_KEY, () => accountIdB64OfHex("0011") ?? "AAA=")).toBe("");
  });
});
