// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A ContactAck on the requester's device (`docs/ux/who-can-find-me.md` section
 * 6.1): the found name is read before the sent record goes, and a Welcome
 * ahead of the ack finds it too. Over the real contact and Sent stores.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./sent-request-intros", () => ({ forgetSentIntro: async () => {} }));

import { base64Encode } from "../auth/account-id";
import { applyContactAck, foundNameForKey } from "./contact-ack";
import type { FoundName } from "./contact-list-model";
import { adoptOneToOne, loadAccepted } from "./local-store";
import { loadSentRequests, recordSentRequest } from "./sent-requests";

const ACCOUNT = "owner";
const key = new Uint8Array(32).fill(5);
const keyB64 = base64Encode(key);
const accountId = new Uint8Array(16).fill(0xaa);
const accountIdHex = "aa".repeat(16);
const ack = { accepterPubkey: key, accepterAccountId: accountId, accepterX25519Pubkey: new Uint8Array(32).fill(6) };

function memoryStorage(): Storage {
  const map = new Map<string, string>();
  return {
    get length() {
      return map.size;
    },
    clear: () => map.clear(),
    getItem: (k) => map.get(k) ?? null,
    key: (i) => [...map.keys()][i] ?? null,
    removeItem: (k) => void map.delete(k),
    setItem: (k, v) => void map.set(k, v),
  };
}

function sessionFiling(live = true): {
  filed: [string, FoundName][];
  session: Parameters<typeof applyContactAck>[2];
} {
  const filed: [string, FoundName][] = [];
  return { filed, session: { current: () => live, fileFoundName: (k, f) => void filed.push([k, f]) } };
}

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage = memoryStorage();
  recordSentRequest(ACCOUNT, {
    recipientUsername: "elin",
    recipientName: "Elin Holm",
    recipientAccountIdHex: accountIdHex,
    sentAtMs: 1,
    recipientKeyB64: keyB64,
  });
});
afterEach(() => {
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
});

describe("applyContactAck", () => {
  it("files the found name, makes the contact, and only then the record goes", () => {
    const { filed, session } = sessionFiling();
    let recordsWhenFiled = -1;
    applyContactAck(ack, ACCOUNT, {
      ...session,
      fileFoundName: (k, f) => {
        recordsWhenFiled = loadSentRequests(ACCOUNT).length;
        session.fileFoundName(k, f);
      },
    });
    expect(filed).toEqual([[keyB64, { displayName: "Elin Holm", username: "elin" }]]);
    expect(recordsWhenFiled).toBe(1);
    expect(loadAccepted(ACCOUNT).map((c) => c.signaturePubkeyB64)).toEqual([keyB64]);
    expect(loadSentRequests(ACCOUNT)).toEqual([]);
  });

  it("a re-delivered ack has no record to read and files nothing", () => {
    applyContactAck(ack, ACCOUNT, sessionFiling().session);
    const { filed, session } = sessionFiling();
    applyContactAck(ack, ACCOUNT, session);
    expect(filed).toEqual([]);
    expect(loadAccepted(ACCOUNT)).toHaveLength(1);
  });

  it("an ack from another key than the one found files nothing, and still makes the contact", () => {
    const { filed, session } = sessionFiling();
    applyContactAck({ ...ack, accepterPubkey: new Uint8Array(32).fill(9) }, ACCOUNT, session);
    expect(filed).toEqual([]);
    expect(loadAccepted(ACCOUNT)).toHaveLength(1);
  });

  it("a session that ended files nothing; the contact and the record are still settled", () => {
    const { filed, session } = sessionFiling(false);
    applyContactAck(ack, ACCOUNT, session);
    expect(filed).toEqual([]);
    expect(loadAccepted(ACCOUNT)).toHaveLength(1);
    expect(loadSentRequests(ACCOUNT)).toEqual([]);
  });
});

describe("a Welcome ahead of the ack", () => {
  it("the row it makes finds the name while the record stands, and the ack still settles it", () => {
    expect(adoptOneToOne({ signatureKey: keyB64, groupIdB64: "G", nowMs: 1 }, ACCOUNT)).toBe(true);
    expect(foundNameForKey(ACCOUNT, keyB64)).toEqual({ displayName: "Elin Holm", username: "elin" });
    const { session } = sessionFiling();
    applyContactAck(ack, ACCOUNT, session);
    expect(loadAccepted(ACCOUNT).map((c) => c.signaturePubkeyB64)).toEqual([keyB64]);
    expect(loadSentRequests(ACCOUNT)).toEqual([]);
  });

  it("nobody was found under a key with no record", () => {
    expect(foundNameForKey(ACCOUNT, base64Encode(new Uint8Array(32).fill(9)))).toBeNull();
  });
});
