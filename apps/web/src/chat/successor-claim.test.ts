// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0065 R.6 - claim, then build: the pure rules of `successor-claim.ts`. */
import { describe, expect, it } from "vitest";

import {
  claimEligible,
  claimOutcome,
  claimVerdict,
  firstEligibleClaim,
  shouldFoundSuccessor,
  unrepairableBy,
  type SuccessorClaim,
} from "./successor-claim";

const OLD = "old-anna";
const ME = "me";
const BOB = "bob";
const CAROL = "carol";
const SUPERSEDED = new Set([OLD]);

function claim(over: Partial<SuccessorClaim> = {}): SuccessorClaim {
  return {
    messageId: 10,
    successorGroupIdB64: "S1",
    senderKeyB64: ME,
    senderWasAdmin: false,
    adminKeysAtReceiptB64: [],
    ...over,
  };
}

describe("unrepairableBy (R.6 (i))", () => {
  it("a one-to-one whose other person was superseded", () => {
    expect(
      unrepairableBy({
        kind: "chat",
        rosterPersonKeysB64: [ME, OLD],
        ownPersonKeyB64: ME,
        ownerKeyB64: ME,
        supersededKeys: SUPERSEDED,
      }),
    ).toBe(OLD);
  });
  it("a group whose OWNER was superseded", () => {
    expect(
      unrepairableBy({
        kind: "group",
        rosterPersonKeysB64: [ME, OLD, BOB],
        ownPersonKeyB64: ME,
        ownerKeyB64: OLD,
        supersededKeys: SUPERSEDED,
      }),
    ).toBe(OLD);
  });
  it("a group where the superseded key is only a member is repairable", () => {
    expect(
      unrepairableBy({
        kind: "group",
        rosterPersonKeysB64: [ME, OLD, BOB],
        ownPersonKeyB64: ME,
        ownerKeyB64: ME,
        supersededKeys: SUPERSEDED,
      }),
    ).toBeNull();
  });
  it("nothing accepted, or no role table: null", () => {
    expect(
      unrepairableBy({
        kind: "chat",
        rosterPersonKeysB64: [ME, BOB],
        ownPersonKeyB64: ME,
        ownerKeyB64: undefined,
        supersededKeys: SUPERSEDED,
      }),
    ).toBeNull();
    expect(
      unrepairableBy({
        kind: "group",
        rosterPersonKeysB64: [ME, OLD],
        ownPersonKeyB64: ME,
        ownerKeyB64: undefined,
        supersededKeys: SUPERSEDED,
      }),
    ).toBeNull();
  });
});

describe("claimEligible (R.6)", () => {
  const group = {
    kind: "group" as const,
    supersededKeyB64: OLD,
    supersededKeys: SUPERSEDED,
    survivingKeyB64: null,
  };
  it("a superseded key is never eligible", () => {
    expect(
      claimEligible({ ...group, senderKeyB64: OLD, senderWasAdmin: true, adminKeysB64: [OLD] }),
    ).toBe(false);
    expect(
      claimEligible({
        kind: "chat",
        senderKeyB64: OLD,
        senderWasAdmin: false,
        adminKeysB64: [],
        supersededKeyB64: OLD,
        supersededKeys: SUPERSEDED,
        survivingKeyB64: ME,
      }),
    ).toBe(false);
  });
  it("one-to-one: any device of the surviving account, and nobody else", () => {
    const chat = {
      kind: "chat" as const,
      senderWasAdmin: false,
      adminKeysB64: [],
      supersededKeyB64: OLD,
      supersededKeys: SUPERSEDED,
      survivingKeyB64: ME,
    };
    expect(claimEligible({ ...chat, senderKeyB64: ME })).toBe(true);
    expect(claimEligible({ ...chat, senderKeyB64: BOB })).toBe(false);
  });
  it("group: an admin other than the superseded owner", () => {
    expect(
      claimEligible({ ...group, senderKeyB64: BOB, senderWasAdmin: true, adminKeysB64: [BOB] }),
    ).toBe(true);
  });
  it("group: a plain member is not eligible while another admin exists", () => {
    expect(
      claimEligible({ ...group, senderKeyB64: CAROL, senderWasAdmin: false, adminKeysB64: [BOB] }),
    ).toBe(false);
  });
  it("group: with no admin but the superseded owner, any member is", () => {
    expect(
      claimEligible({ ...group, senderKeyB64: CAROL, senderWasAdmin: false, adminKeysB64: [] }),
    ).toBe(true);
    expect(
      claimEligible({ ...group, senderKeyB64: CAROL, senderWasAdmin: false, adminKeysB64: [OLD] }),
    ).toBe(true);
  });
});

describe("claimVerdict (the receive rule)", () => {
  const base = {
    kind: "chat" as const,
    groupIdB64: "G",
    held: null,
    unrepairableKeyB64: OLD,
    supersededKeys: SUPERSEDED,
    survivingKeyB64: ME,
  };
  it("accepts the first eligible claim", () => {
    expect(claimVerdict({ ...base, claim: claim() })).toBe("accept");
  });
  it("ignores every later claim for good", () => {
    expect(
      claimVerdict({ ...base, held: claim({ messageId: 10 }), claim: claim({ messageId: 11, successorGroupIdB64: "S2" }) }),
    ).toBe("ignore-later");
    expect(claimVerdict({ ...base, held: claim({ messageId: 10 }), claim: claim({ messageId: 10 }) })).toBe(
      "ignore-later",
    );
  });
  it("an EARLIER claim judged late still wins the journal's order", () => {
    expect(
      claimVerdict({ ...base, held: claim({ messageId: 10 }), claim: claim({ messageId: 7, successorGroupIdB64: "S0" }) }),
    ).toBe("accept");
  });
  it("keeps a claim while this device has no verdict of its own", () => {
    expect(claimVerdict({ ...base, unrepairableKeyB64: null, claim: claim() })).toBe("keep-pending");
  });
  it("never keeps or accepts a claim from the superseded identity's leaves", () => {
    expect(claimVerdict({ ...base, claim: claim({ senderKeyB64: OLD }) })).toBe("drop-ineligible");
    expect(
      claimVerdict({ ...base, unrepairableKeyB64: null, claim: claim({ senderKeyB64: OLD }) }),
    ).toBe("drop-ineligible");
  });
  it("drops a claim from a plain member of a group that has another admin", () => {
    expect(
      claimVerdict({
        ...base,
        kind: "group",
        survivingKeyB64: null,
        claim: claim({ senderKeyB64: CAROL, adminKeysAtReceiptB64: [BOB] }),
      }),
    ).toBe("drop-ineligible");
  });
  it("drops an envelope that names no successor or this very group", () => {
    expect(claimVerdict({ ...base, claim: claim({ successorGroupIdB64: "" }) })).toBe("drop-malformed");
    expect(claimVerdict({ ...base, claim: claim({ successorGroupIdB64: "G" }) })).toBe("drop-malformed");
  });
});

describe("firstEligibleClaim / claimOutcome (step 3)", () => {
  const base = {
    kind: "group" as const,
    groupIdB64: "G",
    unrepairableKeyB64: OLD,
    supersededKeys: SUPERSEDED,
    survivingKeyB64: null,
  };
  const claims = [
    claim({ messageId: 30, senderKeyB64: ME, senderWasAdmin: true, adminKeysAtReceiptB64: [ME, BOB], successorGroupIdB64: "S-me" }),
    claim({ messageId: 12, senderKeyB64: OLD, senderWasAdmin: true, adminKeysAtReceiptB64: [ME, BOB], successorGroupIdB64: "S-old" }),
    claim({ messageId: 20, senderKeyB64: BOB, senderWasAdmin: true, adminKeysAtReceiptB64: [ME, BOB], successorGroupIdB64: "S-bob" }),
    claim({ messageId: 15, senderKeyB64: CAROL, senderWasAdmin: false, adminKeysAtReceiptB64: [ME, BOB], successorGroupIdB64: "S-carol" }),
  ];
  it("the first ELIGIBLE claim in journal order, skipping the superseded sender and the plain member", () => {
    expect(firstEligibleClaim({ ...base, claims })?.successorGroupIdB64).toBe("S-bob");
  });
  it("a founder that finds an earlier eligible claim lost and adopts it", () => {
    const first = firstEligibleClaim({ ...base, claims });
    expect(claimOutcome(30, first)).toEqual({ kind: "lost", winner: claims[2] });
  });
  it("a founder whose claim is the first eligible one won", () => {
    const first = firstEligibleClaim({ ...base, claims: [claims[0] as SuccessorClaim, claims[1] as SuccessorClaim] });
    expect(claimOutcome(30, first)).toEqual({ kind: "won" });
    expect(claimOutcome(30, null)).toEqual({ kind: "won" });
  });
  it("without a verdict nothing is eligible", () => {
    expect(firstEligibleClaim({ ...base, unrepairableKeyB64: null, claims })).toBeNull();
  });
});

describe("shouldFoundSuccessor (steps 1 and 2)", () => {
  const base = {
    kind: "chat" as const,
    unrepairableKeyB64: OLD,
    held: null,
    ownClaimPosted: false,
    membershipEnded: false,
    ownPersonKeyB64: ME,
    ownIsAdmin: false,
    adminKeysB64: [] as string[],
    supersededKeys: SUPERSEDED,
    newIdentityHasDevices: true,
  };
  it("the surviving side of a one-to-one founds", () => {
    expect(shouldFoundSuccessor(base)).toBe(true);
  });
  it("stands down when an eligible claim is already held", () => {
    expect(shouldFoundSuccessor({ ...base, held: claim() })).toBe(false);
  });
  it("does not claim twice", () => {
    expect(shouldFoundSuccessor({ ...base, ownClaimPosted: true })).toBe(false);
  });
  it("waits while the new identity publishes no device (R.8)", () => {
    expect(shouldFoundSuccessor({ ...base, newIdentityHasDevices: false })).toBe(false);
  });
  it("does nothing for a repairable group, an ended membership, or no identity", () => {
    expect(shouldFoundSuccessor({ ...base, unrepairableKeyB64: null })).toBe(false);
    expect(shouldFoundSuccessor({ ...base, membershipEnded: true })).toBe(false);
    expect(shouldFoundSuccessor({ ...base, ownPersonKeyB64: null })).toBe(false);
  });
  it("in an owned group only an admin founds while another admin exists", () => {
    const group = { ...base, kind: "group" as const, adminKeysB64: [BOB] };
    expect(shouldFoundSuccessor(group)).toBe(false);
    expect(shouldFoundSuccessor({ ...group, ownIsAdmin: true, adminKeysB64: [ME, BOB] })).toBe(true);
    expect(shouldFoundSuccessor({ ...group, adminKeysB64: [] })).toBe(true);
  });
});
