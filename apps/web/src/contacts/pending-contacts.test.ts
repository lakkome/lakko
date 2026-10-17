// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  MAX_PENDING_MESSAGES,
  PENDING_STALE_MS,
  decodePendingContacts,
  encodePendingContacts,
  pendingContactForConversation,
  pendingLine,
  pendingNameOf,
  shownPendingContacts,
  withPendingContact,
  withQueuedMessage,
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
};

function withAnna(): PendingContact[] {
  return withPendingContact([], anna);
}

describe("pending contacts (ADR-0079 section 5)", () => {
  it("queues messages in the order written", () => {
    let list = withAnna();
    for (const [i, text] of ["one", "two", "three"].entries()) {
      const next = withQueuedMessage(list, anna.accountIdHex, { id: `m${i}`, text, createdAtMs: 2_000 + i });
      expect(next).not.toBeNull();
      list = next as PendingContact[];
    }
    expect(list[0]?.queue.map((m) => m.text)).toEqual(["one", "two", "three"]);
    // The order survives the store's encoding.
    const back = decodePendingContacts(encodePendingContacts(list));
    expect(back[0]?.queue.map((m) => m.text)).toEqual(["one", "two", "three"]);
  });

  it("refuses an empty message, an unknown contact and a full queue", () => {
    const list = withAnna();
    expect(withQueuedMessage(list, anna.accountIdHex, { id: "x", text: "", createdAtMs: 1 })).toBeNull();
    expect(withQueuedMessage(list, "b".repeat(32), { id: "x", text: "hi", createdAtMs: 1 })).toBeNull();
    let full: PendingContact[] = list;
    for (let i = 0; i < MAX_PENDING_MESSAGES; i += 1) {
      full = withQueuedMessage(full, anna.accountIdHex, { id: `${i}`, text: "x", createdAtMs: i }) as PendingContact[];
    }
    expect(withQueuedMessage(full, anna.accountIdHex, { id: "z", text: "x", createdAtMs: 0 })).toBeNull();
  });

  it("keeps the first Add time and the queue when the same person is added again", () => {
    const queued = withQueuedMessage(withAnna(), anna.accountIdHex, {
      id: "m",
      text: "hi",
      createdAtMs: 5,
    }) as PendingContact[];
    const again = withPendingContact(queued, { ...anna, addedAtMs: 9_999, displayName: "Anna B." });
    expect(again).toHaveLength(1);
    expect(again[0]?.addedAtMs).toBe(1_000);
    expect(again[0]?.displayName).toBe("Anna B.");
    expect(again[0]?.queue).toHaveLength(1);
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
    expect(decodePendingContacts(JSON.stringify({ contacts: [{ accountIdHex: "x" }] }))).toEqual([]);
  });
});
