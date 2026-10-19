// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { HeldSendStore } from "../chat/held-send";
import {
  PENDING_STALE_MS,
  decodePendingContacts,
  encodePendingContacts,
  moveHeldToConversation,
  pendingContactForConversation,
  pendingLine,
  pendingNameOf,
  pendingQueueKey,
  shownPendingContacts,
  withPendingContact,
  withoutPendingContact,
  type PendingContact,
} from "./pending-contacts";

const anna = {
  accountIdHex: "a".repeat(32),
  signaturePubkeyB64: "QU5OQQ==",
  x25519PubkeyB64: "WDI1NTE5",
  displayName: "Anna",
  username: "anna",
  addedAtMs: 1_000,
  linkIdHex: "1".repeat(32),
};

function withAnna(): PendingContact[] {
  return withPendingContact([], anna);
}

describe("pending contacts (ADR-0079 section 5)", () => {
  it("keeps the record without messages and round-trips it through the store encoding", () => {
    const back = decodePendingContacts(encodePendingContacts(withAnna()));
    expect(back).toEqual([anna]);
    expect(Object.keys(back[0] ?? {})).not.toContain("queue");
  });

  it("refuses the version-1 layout that kept messages inside the record", () => {
    const v1 = JSON.stringify({ v: 1, contacts: [{ ...anna, queue: [{ id: "m", text: "hi", createdAtMs: 1 }] }] });
    expect(decodePendingContacts(v1)).toEqual([]);
  });

  it("keys what is written on the inviter's account id, never a conversation id", () => {
    expect(pendingQueueKey(anna)).toBe(anna.accountIdHex);
    expect(pendingQueueKey(anna)).toHaveLength(32);
  });

  it("hands the messages over to the joined conversation in the order written", () => {
    const store = new HeldSendStore<{ text: string }>();
    const conversationHex = "c".repeat(64);
    store.hold(pendingQueueKey(anna), { text: "one" }, 10);
    store.hold(pendingQueueKey(anna), { text: "two" }, 20);
    store.hold(conversationHex, { text: "earlier" }, 5);
    expect(moveHeldToConversation(store, anna, conversationHex, 99)).toBe(2);
    expect(store.count(pendingQueueKey(anna))).toBe(0);
    const moved = store.take(conversationHex);
    expect(moved.map((h) => h.payload.text)).toEqual(["earlier", "one", "two"]);
    // The give-up clock starts at the hand-over.
    expect(moved.map((h) => h.heldAtMs)).toEqual([5, 99, 99]);
    // Nothing left: a second hand-over moves nothing.
    expect(moveHeldToConversation(store, anna, conversationHex, 100)).toBe(0);
  });

  it("keeps the first Add time when the same person is added again", () => {
    const again = withPendingContact(withAnna(), { ...anna, addedAtMs: 9_999, displayName: "Anna B." });
    expect(again).toHaveLength(1);
    expect(again[0]?.addedAtMs).toBe(1_000);
    expect(again[0]?.displayName).toBe("Anna B.");
    expect(withoutPendingContact(again, anna.accountIdHex)).toEqual([]);
  });

  it("shows the 7-day line once a week has passed without an answer", () => {
    const c = withAnna()[0] as PendingContact;
    expect(pendingLine(c, c.addedAtMs + PENDING_STALE_MS - 1)).toBe("waiting");
    expect(pendingLine(c, c.addedAtMs + PENDING_STALE_MS)).toBe("stale");
  });

  it("hides a pending contact once a one-to-one with them exists", () => {
    const list = withAnna();
    expect(shownPendingContacts(list, new Set())).toHaveLength(1);
    expect(shownPendingContacts(list, new Set([anna.signaturePubkeyB64]))).toHaveLength(0);
  });

  it("adopts a joined one-to-one with the person, never a group", () => {
    const list = withAnna();
    const people = [anna.signaturePubkeyB64, "T1dO"];
    expect(pendingContactForConversation(list, { oneToOne: true, personKeysB64: people })?.accountIdHex).toBe(
      anna.accountIdHex,
    );
    expect(pendingContactForConversation(list, { oneToOne: false, personKeysB64: people })).toBeUndefined();
    expect(pendingContactForConversation(list, { oneToOne: true, personKeysB64: ["T1dO"] })).toBeUndefined();
  });

  it("names a pending contact by display name, then @username", () => {
    expect(pendingNameOf(anna)).toBe("Anna");
    expect(pendingNameOf({ displayName: " ", username: "anna" })).toBe("@anna");
    expect(pendingNameOf({ displayName: "", username: "" })).toBeNull();
  });

  it("reads junk as no pending contacts", () => {
    expect(decodePendingContacts("not json")).toEqual([]);
    expect(decodePendingContacts(JSON.stringify({ v: 2, contacts: [{ accountIdHex: "x" }] }))).toEqual([]);
  });
});
