// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0066 section 3.5a rule 2 - grants are keyed on the person, not on the
 * key they arrived under; and the reach book follows the account key
 * (section 3.5, "Reach").
 */
import { describe, expect, it } from "vitest";

import { keysOfPerson } from "../contacts/address-resolver";
import { afterProvenRotation, afterReset, type AccountKeys } from "../contacts/predecessor";
import { EMPTY_REACH_BOOK, bindReachBook, decodeReachBook, encodeReachBook, reachRebind } from "./reach-model";
import {
  EMPTY_RECEIVED_GRANTS,
  bestGrantForPerson,
  delegationsForPerson,
  fileContactGrant,
  fileDelegations,
  fileScopedGrant,
  grantsInOrderForPerson,
  heldClassesForPerson,
} from "./received-grants";

const K0 = "a0";
const K1 = "a1";
const GROUP = "99";
const grant = (wireB64: string, version = 0) => ({ wireB64, version, receivedAtMs: 1 });

describe("grant lookup across a proven rotation", () => {
  const alice: AccountKeys = afterProvenRotation({ currentKeyB64: K0, provenPredecessorsB64: [] }, K1);
  const person = keysOfPerson(K0, [alice], null);

  it("a grant filed under a key that LATER became a proven predecessor is still found", () => {
    const store = fileContactGrant(EMPTY_RECEIVED_GRANTS, K0, grant("old"));
    // Looked up by either key of the person.
    expect(bestGrantForPerson(store, keysOfPerson(K1, [alice], null))?.wireB64).toBe("old");
    expect(bestGrantForPerson(store, person)?.wireB64).toBe("old");
    expect(heldClassesForPerson(store, person).has("contact")).toBe(true);
  });

  it("the grants filed under the current key come first", () => {
    let store = fileContactGrant(EMPTY_RECEIVED_GRANTS, K0, grant("for-old-mailbox", 3));
    store = fileScopedGrant(store, K1, GROUP, "group", grant("for-new-mailbox"));
    expect(grantsInOrderForPerson(store, person, { scopeHex: GROUP }).map((g) => g.wireB64)).toEqual([
      "for-new-mailbox",
      "for-old-mailbox",
    ]);
    expect(bestGrantForPerson(store, person, { scopeHex: GROUP })?.wireB64).toBe("for-new-mailbox");
  });

  it("an UNPROVEN change finds nothing of the old key", () => {
    const reset = afterReset({ currentKeyB64: K0, provenPredecessorsB64: [] }, K1);
    const store = fileContactGrant(EMPTY_RECEIVED_GRANTS, K0, grant("old"));
    expect(bestGrantForPerson(store, keysOfPerson(K1, [reset], null))).toBeNull();
  });

  it("the re-sent profile's grant (same version, another key) replaces the stored one", () => {
    // The version counters survive a rotation; the key does not.
    let store = fileContactGrant(EMPTY_RECEIVED_GRANTS, K0, grant("old-seed", 2));
    store = fileContactGrant(store, K0, grant("new-seed", 2));
    expect(bestGrantForPerson(store, [K0])?.wireB64).toBe("new-seed");
    // The same grant re-sent changes nothing; a lower version is a replay.
    expect(fileContactGrant(store, K0, grant("new-seed", 2))).toBe(store);
    expect(fileContactGrant(store, K0, grant("older", 1))).toBe(store);
  });

  it("a committer delegation is found under any key of the added person", () => {
    const store = fileDelegations(EMPTY_RECEIVED_GRANTS, "5e", K0, { envelopeB64: "env", receivedAtMs: 4 });
    expect(delegationsForPerson(store, person).map((d) => d.envelopeB64)).toEqual(["env"]);
    expect(delegationsForPerson(store, ["bb"])).toEqual([]);
  });
});

describe("the reach book follows the account key (section 3.5, Reach)", () => {
  it("what was granted before the rotation is spent: links and open requests leave the book (3.5a rule 5)", () => {
    const book = {
      ...EMPTY_REACH_BOOK,
      registeredForKeyHex: K0,
      links: [{ linkIdHex: "aa", expiresAtMs: null }],
      replies: [{ scopeHex: "bb", expiresAtMs: 9e15, purpose: "contact-request" as const, peerHex: "cc" }],
      versions: { "contact:": 2 },
    };
    const moved = bindReachBook(book, K1);
    // Their keys derive from the old seed; "the same" scope on the new
    // mailbox would be a key nobody holds.
    expect(moved.links).toEqual([]);
    expect(moved.replies).toEqual([]);
    expect(moved.versions).toEqual({ "contact:": 2 });
    // Without a key change nothing is dropped.
    expect(bindReachBook(book, K0).links).toHaveLength(1);
    expect(bindReachBook(book, K0).replies).toHaveLength(1);
  });

  const registered = { ...EMPTY_REACH_BOOK, registered: { pk: null }, open: true, versions: { "contact:": 4 } };

  it("the decision", () => {
    expect(reachRebind(K1, K1)).toBe("keep");
    expect(reachRebind(K1, undefined)).toBe("stamp");
    expect(reachRebind(K1, K0)).toBe("move");
  });

  it("a book of the bound key is kept as it is", () => {
    const book = { ...registered, registeredForKeyHex: K0 };
    expect(bindReachBook(book, K0)).toBe(book);
  });

  it("an unstamped book becomes the bound key's", () => {
    expect(bindReachBook(registered, K0)).toEqual({ ...registered, registeredForKeyHex: K0 });
  });

  it("after a rotation the registrations are forgotten and everything else is kept", () => {
    const moved = bindReachBook({ ...registered, registeredForKeyHex: K0 }, K1);
    // Nothing is recorded as registered on the NEW mailbox, so the next sync
    // registers every wanted key there; nothing is sent to the old one.
    expect(moved.registered).toEqual({});
    expect(moved.open).toBeNull();
    expect(moved.registeredForKeyHex).toBe(K1);
    expect(moved.versions).toEqual({ "contact:": 4 });
    expect(decodeReachBook(encodeReachBook(moved)).registeredForKeyHex).toBe(K1);
  });
});
