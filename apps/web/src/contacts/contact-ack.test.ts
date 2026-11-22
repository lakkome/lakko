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
import {
  adoptAnsweredOneToOne,
  applyContactAck,
  foundNameForKey,
  retireAnsweredSentRequests,
  sentRecordAskedAcker,
} from "./contact-ack";
import { waitingSentRequests, type FoundName } from "./contact-list-model";
import { requestsToResend } from "./rotation-spent";
import { adoptOneToOne, isRemovedContact, loadAccepted, removeAccepted } from "./local-store";
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

describe("applyContactAck (the ack alone makes no contact, base walk 2026-11-04)", () => {
  it("files the found name, makes NO contact, and keeps the request waiting with the accepter's keys", () => {
    const { filed, session } = sessionFiling();
    applyContactAck(ack, ACCOUNT, session);
    expect(filed).toEqual([[keyB64, { displayName: "Elin Holm", username: "elin" }]]);
    expect(loadAccepted(ACCOUNT)).toEqual([]);
    const [record] = loadSentRequests(ACCOUNT);
    expect(record?.accepterKeyB64).toBe(keyB64);
    expect(record?.accepterX25519B64).toBe(base64Encode(ack.accepterX25519Pubkey));
    // Still listed under Sent as waiting.
    expect(waitingSentRequests(loadSentRequests(ACCOUNT), new Set())).toHaveLength(1);
  });

  it("an ack for a request this device never sent changes nothing", () => {
    applyContactAck({ ...ack, accepterAccountId: new Uint8Array(16).fill(0xbb) }, ACCOUNT, sessionFiling().session);
    expect(loadAccepted(ACCOUNT)).toEqual([]);
    expect(loadSentRequests(ACCOUNT)[0]?.accepterKeyB64).toBeUndefined();
  });

  it("a session that ended files nothing; the keys are still kept", () => {
    const { filed, session } = sessionFiling(false);
    applyContactAck(ack, ACCOUNT, session);
    expect(filed).toEqual([]);
    expect(loadSentRequests(ACCOUNT)[0]?.accepterKeyB64).toBe(keyB64);
  });
});

describe("joining the one-to-one makes the contact and retires the request", () => {
  it("after the ack: the row is made by the one-to-one, the X25519 key carried over, the request gone", () => {
    applyContactAck(ack, ACCOUNT, sessionFiling().session);
    expect(adoptOneToOne({ signatureKey: keyB64, groupIdB64: "G", nowMs: 1 }, ACCOUNT)).toBe(true);
    expect(retireAnsweredSentRequests(ACCOUNT, keyB64, null)).toBe(1);
    const [contact] = loadAccepted(ACCOUNT);
    expect(contact?.groupIdB64).toBe("G");
    expect(contact?.x25519PubkeyB64).toBe(base64Encode(ack.accepterX25519Pubkey));
    expect(contact?.accountIdHex).toBe(accountIdHex);
    expect(loadSentRequests(ACCOUNT)).toEqual([]);
  });

  it("a Welcome ahead of the ack: the row finds the name while the record stands, and joining retires it", () => {
    expect(adoptOneToOne({ signatureKey: keyB64, groupIdB64: "G", nowMs: 1 }, ACCOUNT)).toBe(true);
    expect(foundNameForKey(ACCOUNT, keyB64)).toEqual({ displayName: "Elin Holm", username: "elin" });
    expect(retireAnsweredSentRequests(ACCOUNT, keyB64, null)).toBe(1);
    expect(loadAccepted(ACCOUNT).map((c) => c.signaturePubkeyB64)).toEqual([keyB64]);
    expect(loadSentRequests(ACCOUNT)).toEqual([]);
  });

  it("a one-to-one with someone else retires nothing", () => {
    expect(retireAnsweredSentRequests(ACCOUNT, base64Encode(new Uint8Array(32).fill(9)), null)).toBe(0);
    expect(loadSentRequests(ACCOUNT)).toHaveLength(1);
  });

  it("an acked request is not re-sent after a key rotation", () => {
    applyContactAck(ack, ACCOUNT, sessionFiling().session);
    expect(requestsToResend(loadSentRequests(ACCOUNT), 10)).toEqual([]);
  });

  it("nobody was found under a key with no record", () => {
    expect(foundNameForKey(ACCOUNT, base64Encode(new Uint8Array(32).fill(9)))).toBeNull();
  });
});

describe("the silent answer: a removed contact asked again (base scenario 7, 2026-11-22)", () => {
  function sessionWithOneToOne(groupIdB64: string | null): Parameters<typeof applyContactAck>[2] {
    return { current: () => true, fileFoundName: () => {}, liveOneToOneWith: () => groupIdB64 };
  }

  it("an ack from a removed person this device holds a one-to-one with puts them back, with it, and retires Sent", () => {
    // Was a contact with chat G, removed, asked again from search.
    expect(adoptOneToOne({ signatureKey: keyB64, groupIdB64: "G", nowMs: 1 }, ACCOUNT)).toBe(true);
    removeAccepted(keyB64, ACCOUNT);
    expect(isRemovedContact(keyB64, ACCOUNT)).toBe(true);
    expect(applyContactAck(ack, ACCOUNT, sessionWithOneToOne("G"))).toBe("restored");
    const [contact] = loadAccepted(ACCOUNT);
    expect(contact?.signaturePubkeyB64).toBe(keyB64);
    expect(contact?.groupIdB64).toBe("G");
    expect(contact?.accountIdHex).toBe(accountIdHex);
    expect(contact?.x25519PubkeyB64).toBe(base64Encode(ack.accepterX25519Pubkey));
    expect(isRemovedContact(keyB64, ACCOUNT)).toBe(false);
    expect(loadSentRequests(ACCOUNT)).toEqual([]);
  });

  it("without a one-to-one on this device the caller founds one; until then nothing is made", () => {
    removeAccepted(keyB64, ACCOUNT);
    expect(applyContactAck(ack, ACCOUNT, sessionWithOneToOne(null))).toBe("needs-one-to-one");
    expect(loadAccepted(ACCOUNT)).toEqual([]);
    expect(isRemovedContact(keyB64, ACCOUNT)).toBe(true);
    expect(waitingSentRequests(loadSentRequests(ACCOUNT), new Set())).toHaveLength(1);
    // The founded one-to-one makes the contact and retires the request.
    expect(adoptAnsweredOneToOne(ACCOUNT, keyB64, accountIdHex, "NEW")).toBe(true);
    const [contact] = loadAccepted(ACCOUNT);
    expect(contact?.groupIdB64).toBe("NEW");
    expect(contact?.x25519PubkeyB64).toBe(base64Encode(ack.accepterX25519Pubkey));
    expect(isRemovedContact(keyB64, ACCOUNT)).toBe(false);
    expect(loadSentRequests(ACCOUNT)).toEqual([]);
  });

  it("a session that cannot see conversations asks for nothing", () => {
    expect(applyContactAck(ack, ACCOUNT, sessionFiling().session)).toBe("none");
  });

  it("a one-to-one with a person this device never asked brings nobody back", () => {
    const other = new Uint8Array(32).fill(9);
    const otherB64 = base64Encode(other);
    removeAccepted(otherB64, ACCOUNT);
    const stray = { ...ack, accepterPubkey: other, accepterAccountId: new Uint8Array(16).fill(0xbb) };
    expect(applyContactAck(stray, ACCOUNT, sessionWithOneToOne("H"))).toBe("none");
    expect(applyContactAck(stray, ACCOUNT, sessionWithOneToOne(null))).toBe("none");
    expect(loadAccepted(ACCOUNT)).toEqual([]);
    expect(isRemovedContact(otherB64, ACCOUNT)).toBe(true);
    expect(loadSentRequests(ACCOUNT)).toHaveLength(1);
  });

  it("an ack naming the asked account under another key restores nobody", () => {
    const other = new Uint8Array(32).fill(9);
    const forged = { ...ack, accepterPubkey: other };
    expect(applyContactAck(forged, ACCOUNT, sessionWithOneToOne("H"))).toBe("none");
    expect(applyContactAck(forged, ACCOUNT, sessionWithOneToOne(null))).toBe("none");
    expect(loadAccepted(ACCOUNT)).toEqual([]);
  });

  it("sentRecordAskedAcker: the key, or the account when the record names no key", () => {
    const rec = { recipientAccountIdHex: accountIdHex, recipientKeyB64: keyB64 };
    expect(sentRecordAskedAcker(rec, keyB64, "bb".repeat(16))).toBe(true);
    expect(sentRecordAskedAcker(rec, "other", accountIdHex)).toBe(false);
    expect(sentRecordAskedAcker({ recipientAccountIdHex: accountIdHex }, "any", accountIdHex)).toBe(true);
    expect(sentRecordAskedAcker({ recipientAccountIdHex: accountIdHex }, "any", "bb".repeat(16))).toBe(false);
  });
});
