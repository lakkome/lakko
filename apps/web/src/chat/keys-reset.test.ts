// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 phase 2 (`docs/ux/identity-recovery.md` section 16) — the pure
 * decisions of `keys-reset.ts`: which branch a shared group falls in, the
 * `applied` test, when the eviction is proposed, what the row says, what is
 * blocked, the add-arm suppression, the bubble warning, the late-message
 * drop.
 */
import { describe, expect, it } from "vitest";

import { base64Encode } from "../auth/account-id";
import {
  blockedActions,
  bubbleWarningFor,
  dropsLateMessage,
  evictionApplied,
  leavesOfPreviousKey,
  memberRowResetFlag,
  readmissionAddEligible,
  resetGroupBranch,
  shouldProposeEviction,
} from "./keys-reset";
import type { RosterLeaf } from "./person-key";

const OLD = new Uint8Array(32).fill(0x0a);
const OLD_B64 = base64Encode(OLD);
const NEW = new Uint8Array(32).fill(0x0b);
const NEW_B64 = base64Encode(NEW);
const OTHER = new Uint8Array(32).fill(0x0c);
const OTHER_B64 = base64Encode(OTHER);

function leaf(leafIndex: number, account: Uint8Array, device: number): RosterLeaf {
  return {
    leafIndex,
    signaturePublicKey: new Uint8Array(32).fill(device),
    accountKey: account,
  };
}

describe("resetGroupBranch — the three branches per shared group (16.3 / 16.4 / 16.5)", () => {
  it("a one-to-one closes, whatever the role table says", () => {
    expect(
      resetGroupBranch({ kind: "chat", ownerKeyB64: undefined, previousKeyB64: OLD_B64, ownRole: null }),
    ).toBe("one-to-one");
    expect(
      resetGroupBranch({ kind: "chat", ownerKeyB64: OLD_B64, previousKeyB64: OLD_B64, ownRole: "admin" }),
    ).toBe("one-to-one");
  });

  it("a group with no role table only flags: there is no proposal path", () => {
    expect(
      resetGroupBranch({ kind: "group", ownerKeyB64: undefined, previousKeyB64: OLD_B64, ownRole: "owner" }),
    ).toBe("flag-only");
  });

  it("a group the old identity OWNED is never evicted from (owner_survives): Recreate group", () => {
    for (const ownRole of ["owner", "admin", "member", null] as const) {
      expect(
        resetGroupBranch({ kind: "group", ownerKeyB64: OLD_B64, previousKeyB64: OLD_B64, ownRole }),
      ).toBe("owned-by-reset");
    }
  });

  it("an owner or admin of a group the old identity does not own evicts", () => {
    expect(
      resetGroupBranch({ kind: "group", ownerKeyB64: OTHER_B64, previousKeyB64: OLD_B64, ownRole: "owner" }),
    ).toBe("evict");
    expect(
      resetGroupBranch({ kind: "group", ownerKeyB64: OTHER_B64, previousKeyB64: OLD_B64, ownRole: "admin" }),
    ).toBe("evict");
  });

  it("a plain member, or a torn role read, proposes nothing", () => {
    expect(
      resetGroupBranch({ kind: "group", ownerKeyB64: OTHER_B64, previousKeyB64: OLD_B64, ownRole: "member" }),
    ).toBe("flag-only");
    expect(
      resetGroupBranch({ kind: "group", ownerKeyB64: OTHER_B64, previousKeyB64: OLD_B64, ownRole: null }),
    ).toBe("flag-only");
  });
});

describe("leavesOfPreviousKey / evictionApplied — the `applied` test on the person key", () => {
  const roster = [leaf(0, OTHER, 1), leaf(1, OLD, 2), leaf(2, OLD, 3), leaf(3, NEW, 4)];

  it("finds every device leaf of the old key, none of the new one", () => {
    expect(leavesOfPreviousKey(roster, OLD_B64)).toEqual([1, 2]);
    expect(leavesOfPreviousKey(roster, NEW_B64)).toEqual([3]);
  });

  it("is applied once no leaf resolves to the old key", () => {
    expect(evictionApplied(roster, OLD_B64)).toBe(false);
    expect(evictionApplied([leaf(0, OTHER, 1), leaf(3, NEW, 4)], OLD_B64)).toBe(true);
  });

  it("a torn roster is never applied: a device that cannot read the roster claims no removal", () => {
    expect(evictionApplied(undefined, OLD_B64)).toBe(false);
  });

  it("compares on the person key, never the leaf index (a vacated leaf is reused)", () => {
    // Leaf 1 now holds somebody else: the old key is gone.
    expect(evictionApplied([leaf(0, OTHER, 1), leaf(1, NEW, 9)], OLD_B64)).toBe(true);
  });
});

describe("shouldProposeEviction", () => {
  const base = { branch: "evict" as const, applied: false, alreadyPending: false, membershipEnded: false };

  it("proposes exactly on the evict branch, not applied, nothing pending, still a member", () => {
    expect(shouldProposeEviction(base)).toBe(true);
  });

  it("never proposes in the owned group, a one-to-one or a flag-only group", () => {
    expect(shouldProposeEviction({ ...base, branch: "owned-by-reset" })).toBe(false);
    expect(shouldProposeEviction({ ...base, branch: "one-to-one" })).toBe(false);
    expect(shouldProposeEviction({ ...base, branch: "flag-only" })).toBe(false);
  });

  it("does not propose twice, after it applied, or after this device's membership ended", () => {
    expect(shouldProposeEviction({ ...base, alreadyPending: true })).toBe(false);
    expect(shouldProposeEviction({ ...base, applied: true })).toBe(false);
    expect(shouldProposeEviction({ ...base, membershipEnded: true })).toBe(false);
  });
});

describe("memberRowResetFlag — the row's words (16.3 / 16.5)", () => {
  it("`Being removed.` only where the eviction is proposed; `Account keys reset.` elsewhere", () => {
    expect(memberRowResetFlag({ branch: "evict", applied: false })).toBe("being-removed");
    expect(memberRowResetFlag({ branch: "owned-by-reset", applied: false })).toBe("flag");
    expect(memberRowResetFlag({ branch: "flag-only", applied: false })).toBe("flag");
  });

  it("nothing once the old key left the roster (the row is gone)", () => {
    expect(memberRowResetFlag({ branch: "evict", applied: true })).toBeNull();
  });
});

describe("blockedActions — the 16.1 table", () => {
  it("everything listed is blocked for a flagged contact and nothing for a normal one", () => {
    expect(blockedActions(true)).toEqual({
      startChat: true,
      addToGroup: true,
      approveJoin: true,
      includeInRecreate: true,
      call: true,
    });
    expect(Object.values(blockedActions(false)).every((v) => v === false)).toBe(true);
  });
});

describe("readmissionAddEligible — the add-arm suppression (16.8)", () => {
  it("only an account whose published key IS the pin is looked up for devices to admit", () => {
    expect(readmissionAddEligible(false, "current")).toBe(true);
  });

  it("a flagged contact is never admitted, whatever the check said", () => {
    expect(readmissionAddEligible(true, "current")).toBe(false);
    expect(readmissionAddEligible(true, "reset")).toBe(false);
  });

  it("a reset, a change the log does not confirm, an absent row and a failed check all stand aside", () => {
    expect(readmissionAddEligible(false, "reset")).toBe(false);
    expect(readmissionAddEligible(false, "changed")).toBe(false);
    expect(readmissionAddEligible(false, "absent")).toBe(false);
    expect(readmissionAddEligible(false, null)).toBe(false);
  });
});

describe("bubbleWarningFor — the warning under a sender's name (16.1)", () => {
  const pending = new Map([[NEW_B64, "row-key"]]);

  it("names the contact when the sender is a flagged contact's NEW key", () => {
    expect(bubbleWarningFor(NEW_B64, pending)).toBe("row-key");
  });

  it("never for the old key (it is the contact's row), a stranger, or an unattributable sender", () => {
    expect(bubbleWarningFor(OLD_B64, pending)).toBeNull();
    expect(bubbleWarningFor(OTHER_B64, pending)).toBeNull();
    expect(bubbleWarningFor(undefined, pending)).toBeNull();
  });

  it("never once the contact is verified (the record is gone)", () => {
    expect(bubbleWarningFor(NEW_B64, new Map())).toBeNull();
  });
});

describe("dropsLateMessage — a late message into a closed one-to-one (16.4)", () => {
  it("drops a chat message and an op before they are stored, only while closed", () => {
    expect(dropsLateMessage(true, "application")).toBe(true);
    expect(dropsLateMessage(true, "op")).toBe(true);
    expect(dropsLateMessage(false, "application")).toBe(false);
  });

  it("lets a commit through: the person's own devices still join and retire", () => {
    expect(dropsLateMessage(true, "system")).toBe(false);
    expect(dropsLateMessage(true, "membershipProposal")).toBe(false);
  });
});
