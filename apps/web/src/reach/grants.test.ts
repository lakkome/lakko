// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 6 — which received grants are kept, under whose key, and
 * which one a write uses. Every "from" below is the sender's account key as
 * resolved at receipt (C-68).
 */
import { describe, expect, it } from "vitest";

import { acceptReceivedGrant, type GrantReceipt } from "./grant-rules";
import { foldProfileGrants } from "./grant-intake";
import { chooseWriteAuth, accountKeyOfLeaf } from "./mailbox-write";
import {
  EMPTY_RECEIVED_GRANTS,
  bestGrantFor,
  canFind,
  delegationsFor,
  dropContactGrant,
  dropScope,
  fileContactGrant,
  fileDelegations,
  fileScopedGrant,
  heldClasses,
  pruneDelegations,
  withdrawMissingScopedGrants,
  decodeReceivedGrants,
  encodeReceivedGrants,
} from "./received-grants";

const OWN = "00";
const ALICE = "aa";

function receipt(over: Partial<GrantReceipt> = {}): GrantReceipt {
  return {
    carrier: { kind: "profile", conversation: "group" },
    senderAccountKeyHex: ALICE,
    ownAccountKeyHex: OWN,
    senderIsMember: true,
    ...over,
  };
}

const g = (wire: string, version = 0) => ({ wireB64: wire, version, receivedAtMs: 1 });

describe("the acceptance rule", () => {
  it("keeps a contact grant only from the other participant of a one-to-one", () => {
    expect(acceptReceivedGrant("contact", receipt({ carrier: { kind: "profile", conversation: "one-to-one" } }))).toBe(true);
    expect(acceptReceivedGrant("contact", receipt())).toBe(false);
    expect(acceptReceivedGrant("contact", receipt({ carrier: { kind: "member-card" } }))).toBe(false);
  });

  it("keeps group and member grants only from members, a group grant never in a one-to-one", () => {
    expect(acceptReceivedGrant("group", receipt())).toBe(true);
    expect(acceptReceivedGrant("group", receipt({ carrier: { kind: "profile", conversation: "one-to-one" } }))).toBe(false);
    expect(acceptReceivedGrant("member", receipt({ carrier: { kind: "profile", conversation: "one-to-one" } }))).toBe(true);
    expect(acceptReceivedGrant("member", receipt({ senderIsMember: false }))).toBe(false);
  });

  it("keeps a channel grant only from a member card", () => {
    expect(acceptReceivedGrant("channel", receipt({ carrier: { kind: "member-card" } }))).toBe(true);
    expect(acceptReceivedGrant("channel", receipt())).toBe(false);
  });

  it("never keeps link, reply or fof grants from a profile, nor anything unattributed or our own", () => {
    for (const cls of ["link", "reply", "fof"] as const) expect(acceptReceivedGrant(cls, receipt())).toBe(false);
    expect(acceptReceivedGrant("group", receipt({ senderAccountKeyHex: null }))).toBe(false);
    expect(acceptReceivedGrant("group", receipt({ senderAccountKeyHex: OWN }))).toBe(false);
  });
});

describe("the received-grant store", () => {
  it("files under the resolved sender key; a higher version replaces, a replay does not", () => {
    let s = fileScopedGrant(EMPTY_RECEIVED_GRANTS, ALICE, "g1", "group", g("v1", 1));
    s = fileScopedGrant(s, ALICE, "g1", "group", g("v0", 0));
    expect(s.people[ALICE]?.scopes.g1?.group?.wireB64).toBe("v1");
    s = fileScopedGrant(s, ALICE, "g1", "group", g("v2", 2));
    expect(s.people[ALICE]?.scopes.g1?.group?.wireB64).toBe("v2");
  });

  it("a profile without a grant it used to carry withdraws it", () => {
    let s = fileScopedGrant(EMPTY_RECEIVED_GRANTS, ALICE, "g1", "group", g("grp"));
    s = fileScopedGrant(s, ALICE, "g1", "member", g("mem"));
    s = withdrawMissingScopedGrants(s, ALICE, "g1", new Set(["member"]));
    expect(heldClasses(s, ALICE, "g1")).toEqual(new Set(["member"]));
  });

  it("prefers contact, then a group key, then a channel key; a member key only for a Welcome into its scope", () => {
    let s = fileScopedGrant(EMPTY_RECEIVED_GRANTS, ALICE, "g1", "member", g("mem"));
    expect(bestGrantFor(s, ALICE)).toBeNull();
    expect(canFind(s, ALICE)).toBe(false);
    expect(bestGrantFor(s, ALICE, { scopeHex: "g1", allowMember: true })?.wireB64).toBe("mem");
    expect(bestGrantFor(s, ALICE, { scopeHex: "g2", allowMember: true })).toBeNull();
    s = fileScopedGrant(s, ALICE, "c1", "channel", g("chan"));
    expect(bestGrantFor(s, ALICE)?.wireB64).toBe("chan");
    s = fileScopedGrant(s, ALICE, "g2", "group", g("grp"));
    expect(bestGrantFor(s, ALICE)?.wireB64).toBe("grp");
    s = fileContactGrant(s, ALICE, g("con"));
    expect(bestGrantFor(s, ALICE)?.wireB64).toBe("con");
    expect(canFind(s, ALICE)).toBe(true);
    s = dropContactGrant(s, ALICE);
    expect(bestGrantFor(s, ALICE)?.wireB64).toBe("grp");
    s = dropScope(s, "g2");
    expect(bestGrantFor(s, ALICE)?.wireB64).toBe("chan");
  });

  it("keeps the newest delegation envelope per (sender, added) and prunes old ones", () => {
    let s = fileDelegations(EMPTY_RECEIVED_GRANTS, "p1", "add", { envelopeB64: "old", receivedAtMs: 1 });
    s = fileDelegations(s, "p1", "add", { envelopeB64: "new", receivedAtMs: 5 });
    s = fileDelegations(s, "p1", "add", { envelopeB64: "stale", receivedAtMs: 3 });
    s = fileDelegations(s, "p2", "add", { envelopeB64: "other", receivedAtMs: 4 });
    expect(delegationsFor(s, "add").map((d) => d.envelopeB64)).toEqual(["new", "other"]);
    expect(delegationsFor(pruneDelegations(s, 10, 5), "add").map((d) => d.envelopeB64)).toEqual(["new"]);
  });

  it("round-trips", () => {
    const s = fileContactGrant(EMPTY_RECEIVED_GRANTS, ALICE, g("con", 3));
    expect(decodeReceivedGrants(encodeReceivedGrants(s))).toEqual(s);
    expect(decodeReceivedGrants("{")).toEqual(EMPTY_RECEIVED_GRANTS);
  });
});

describe("folding a profile's grants", () => {
  const args = (over: Partial<Parameters<typeof foldProfileGrants>[1]> = {}) => ({
    senderHex: ALICE,
    ownHex: OWN,
    scopeHex: "g1",
    oneToOne: false,
    grants: [
      { classByte: 1, version: 0, wireB64: "contact" },
      { classByte: 4, version: 0, wireB64: "group" },
      { classByte: 5, version: 0, wireB64: "member" },
      { classByte: 3, version: 0, wireB64: "reply" },
    ],
    nowMs: 9,
    ...over,
  });

  it("keeps group and member in a group, never the contact or reply grant", () => {
    const s = foldProfileGrants(EMPTY_RECEIVED_GRANTS, args());
    expect(heldClasses(s, ALICE)).toEqual(new Set(["group", "member"]));
  });

  it("keeps contact and member in a one-to-one, never the group grant", () => {
    const s = foldProfileGrants(EMPTY_RECEIVED_GRANTS, args({ oneToOne: true }));
    expect(heldClasses(s, ALICE)).toEqual(new Set(["contact", "member"]));
  });

  it("a later profile without the group grant withdraws it (the sender narrowed)", () => {
    let s = foldProfileGrants(EMPTY_RECEIVED_GRANTS, args());
    s = foldProfileGrants(s, args({ grants: [{ classByte: 5, version: 0, wireB64: "member" }] }));
    expect(heldClasses(s, ALICE, "g1")).toEqual(new Set(["member"]));
  });

  it("ignores our own profile echo", () => {
    expect(foldProfileGrants(EMPTY_RECEIVED_GRANTS, args({ senderHex: OWN }))).toEqual(EMPTY_RECEIVED_GRANTS);
  });
});

describe("the write authorisation", () => {
  const facts = { isOwnAccount: false, hasExplicit: false, kept: null, hasDelegation: false };

  it("own account first, then the explicit grant, then a kept one, then a delegation", () => {
    expect(chooseWriteAuth({ ...facts, isOwnAccount: true, hasExplicit: true })).toBe("own");
    expect(chooseWriteAuth({ ...facts, hasExplicit: true, kept: g("k"), hasDelegation: true })).toBe("explicit");
    expect(chooseWriteAuth({ ...facts, kept: g("k"), hasDelegation: true })).toBe("kept");
    expect(chooseWriteAuth({ ...facts, hasDelegation: true })).toBe("delegation");
    expect(chooseWriteAuth(facts)).toBe("none");
  });

  it("addresses a leaf's Welcome to its account key, or the leaf itself when unbound", () => {
    const leaf = new Uint8Array(32).fill(1);
    const account = new Uint8Array(32).fill(2);
    expect(accountKeyOfLeaf([{ signaturePublicKey: leaf, accountKey: account }], leaf)).toEqual(account);
    expect(accountKeyOfLeaf([{ signaturePublicKey: leaf }], leaf)).toEqual(leaf);
    expect(accountKeyOfLeaf([], leaf)).toEqual(leaf);
  });
});
