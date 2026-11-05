// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The rule "a person this account has a one-to-one conversation with IS a
 * contact" (`adopt-one-to-one.ts`), pure, against fixed lists.
 */
import { describe, expect, it } from "vitest";
import { adoptOneToOnePeer, adoptablePeerOf } from "./adopt-one-to-one";
import type { AcceptedContact } from "./local-store";
import { oneToOnePeerKey } from "../chat/person-key";

const ALICE = "YWxpY2Uta2V5LTAwMDAwMDAwMDAwMDAwMDAwMDA=";
const BOB = "Ym9iLWtleS0wMDAwMDAwMDAwMDAwMDAwMDAwMDAw";
const ME = "bWUta2V5LTAwMDAwMDAwMDAwMDAwMDAwMDAwMDAw";
const GROUP = "Z3JvdXAtaWQtMDAwMDAwMDAwMDAwMDAwMDAwMDA=";
const OTHER_GROUP = "b3RoZXItZ3JvdXAtaWQtMDAwMDAwMDAwMDAwMDA=";
const ACCOUNT = "0123456789abcdef0123456789abcdef";
const NOW = 1_780_000_000_000;

const alice: AcceptedContact = {
  signaturePubkeyB64: ALICE,
  displayName: "Alice",
  acceptedAtMs: 1,
  accountIdHex: "fedcba9876543210fedcba9876543210",
  groupIdB64: OTHER_GROUP,
};

describe("adoptOneToOnePeer", () => {
  // ADR-0074 section 3.7: a person met after a rotation asks with their
  // newest key while the conversation is under the key their leaves are bound
  // to. Seen as two rows of one person on iOS on 2026-11-01.
  it("adds no second row for the conversation key of a person", () => {
    const { groupIdB64: _, ...noChat } = alice;
    const got = adoptOneToOnePeer(
      [noChat],
      { signatureKey: BOB, groupIdB64: GROUP, nowMs: NOW },
      () => true,
    );
    expect(got).toEqual([{ ...noChat, groupIdB64: GROUP, previousSignaturePubkeysB64: [BOB] }]);
  });

  it("leaves a row that knows the conversation key alone", () => {
    const held = { ...alice, previousSignaturePubkeysB64: [BOB] };
    expect(
      adoptOneToOnePeer([held], { signatureKey: BOB, groupIdB64: GROUP, nowMs: NOW }, () => true),
    ).toBeNull();
  });

  it("adds a row keyed on the peer's key when no contact has it", () => {
    const got = adoptOneToOnePeer([alice], {
      signatureKey: BOB,
      accountIdHex: ACCOUNT,
      groupIdB64: GROUP,
      nowMs: NOW,
    });
    expect(got).not.toBeNull();
    expect(got).toHaveLength(2);
    expect(got?.[0]).toEqual(alice);
    expect(got?.[1]).toEqual({
      signaturePubkeyB64: BOB,
      displayName: "",
      acceptedAtMs: NOW,
      accountIdHex: ACCOUNT,
      groupIdB64: GROUP,
    });
  });

  it("adds a row without an account id when no profile names one", () => {
    const got = adoptOneToOnePeer([], { signatureKey: BOB, groupIdB64: GROUP, nowMs: NOW });
    expect(got?.[0]).toEqual({
      signaturePubkeyB64: BOB,
      displayName: "",
      acceptedAtMs: NOW,
      groupIdB64: GROUP,
    });
    expect(got?.[0]).not.toHaveProperty("accountIdHex");
  });

  it("the key of the row IS the peer's key: stable per key, no id of its own", () => {
    const a = adoptOneToOnePeer([], { signatureKey: BOB, groupIdB64: GROUP, nowMs: NOW });
    const b = adoptOneToOnePeer([], { signatureKey: BOB, groupIdB64: GROUP, nowMs: NOW + 5 });
    expect(a?.[0]?.signaturePubkeyB64).toBe(b?.[0]?.signaturePubkeyB64);
    expect(a?.[0]).not.toHaveProperty("id");
    // Adopting the same peer into the list it was adopted into is a no-op.
    expect(
      adoptOneToOnePeer(a as AcceptedContact[], {
        signatureKey: BOB,
        groupIdB64: GROUP,
        nowMs: NOW + 9,
      }),
    ).toBeNull();
  });

  it("completes a row's missing group id and keeps its display name", () => {
    const named: AcceptedContact = {
      signaturePubkeyB64: BOB,
      displayName: "Bobby",
      acceptedAtMs: 7,
      x25519PubkeyB64: "eDI1NTE5",
    };
    const got = adoptOneToOnePeer([alice, named], {
      signatureKey: BOB,
      groupIdB64: GROUP,
      nowMs: NOW,
    });
    expect(got?.[1]).toEqual({ ...named, groupIdB64: GROUP });
    expect(got?.[1]?.displayName).toBe("Bobby");
    expect(got?.[1]?.acceptedAtMs).toBe(7);
  });

  it("completes a missing account id without touching a recorded chat", () => {
    const linked: AcceptedContact = {
      signaturePubkeyB64: BOB,
      displayName: "",
      acceptedAtMs: 7,
      groupIdB64: OTHER_GROUP,
    };
    const got = adoptOneToOnePeer([linked], {
      signatureKey: BOB,
      accountIdHex: ACCOUNT,
      groupIdB64: GROUP,
      nowMs: NOW,
    });
    expect(got?.[0]).toEqual({ ...linked, accountIdHex: ACCOUNT });
    expect(got?.[0]?.groupIdB64).toBe(OTHER_GROUP);
  });

  it("leaves a complete row alone, even with a different chat or account id", () => {
    const got = adoptOneToOnePeer([alice], {
      signatureKey: ALICE,
      accountIdHex: ACCOUNT,
      groupIdB64: GROUP,
      nowMs: NOW,
    });
    expect(got).toBeNull();
  });

  it("returns the other rows untouched and in order", () => {
    const bob: AcceptedContact = { signaturePubkeyB64: BOB, displayName: "B", acceptedAtMs: 2 };
    const got = adoptOneToOnePeer([alice, bob], {
      signatureKey: BOB,
      groupIdB64: GROUP,
      nowMs: NOW,
    });
    expect(got?.[0]).toBe(alice);
    expect(got?.[1]).toEqual({ ...bob, groupIdB64: GROUP });
  });
});

describe("oneToOnePeerKey", () => {
  it("names the other person of a two-person roster", () => {
    expect(oneToOnePeerKey([ME, BOB], ME)).toBe(BOB);
    expect(oneToOnePeerKey([BOB, ME], ME)).toBe(BOB);
  });

  it("is null for a group, for a roster without the own account, or with no own key", () => {
    expect(oneToOnePeerKey([ME, BOB, ALICE], ME)).toBeNull();
    expect(oneToOnePeerKey([ALICE, BOB], ME)).toBeNull();
    expect(oneToOnePeerKey([ME], ME)).toBeNull();
    expect(oneToOnePeerKey([ME, BOB], null)).toBeNull();
  });
});

describe("a contact is linked only to their one-to-one (seen live 2026-11-05)", () => {
  it("a GROUP that holds two people makes no contact and no link", () => {
    // NewGroup founds with its first pick and adds the rest afterwards, so a
    // group passes through two people.
    expect(adoptablePeerOf({ isOneToOne: false, people: [ME, BOB], ownKeyB64: ME })).toBeNull();
    expect(adoptablePeerOf({ isOneToOne: true, people: [ME, BOB], ownKeyB64: ME })).toBe(BOB);
    expect(adoptablePeerOf({ isOneToOne: true, people: [ME, BOB, ALICE], ownKeyB64: ME })).toBeNull();
    expect(adoptablePeerOf({ isOneToOne: true, people: [ME, BOB], ownKeyB64: null })).toBeNull();
  });

  it("a row linked to something that is not their one-to-one is relinked to it", () => {
    const row: AcceptedContact = { signaturePubkeyB64: BOB, displayName: "Bob", acceptedAtMs: 1, groupIdB64: GROUP };
    const next = adoptOneToOnePeer(
      [row],
      { signatureKey: BOB, groupIdB64: OTHER_GROUP, nowMs: 2 },
      undefined,
      (id) => id === OTHER_GROUP,
    );
    expect(next).not.toBeNull();
    expect(next?.[0]?.groupIdB64).toBe(OTHER_GROUP);
    expect(next?.[0]?.displayName).toBe("Bob");
  });

  it("a row whose link is still their one-to-one keeps it", () => {
    const row: AcceptedContact = { signaturePubkeyB64: BOB, displayName: "Bob", acceptedAtMs: 1, groupIdB64: GROUP };
    expect(
      adoptOneToOnePeer([row], { signatureKey: BOB, groupIdB64: OTHER_GROUP, nowMs: 2 }, undefined, () => true),
    ).toBeNull();
  });
});
