// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0073 R.7 / R.8 - the pure decisions of `reset-repair.ts`. */
import { describe, expect, it } from "vitest";

import {
  adminRestorePlan,
  adminRestoreStep,
  deviceSwapAcrossCommits,
  DEVICE_SWAP_PAIR_WINDOW_MS,
  dropsSupersededSender,
  leavesOfKey,
  mayAddResetIdentity,
  membersChangedSuppressed,
  messagesToResend,
  peopleByAccount,
  repairPlan,
  RESEND_REPLY_TARGET,
  resendOwnMessagesAfterReset,
  resendParts,
  resendRemaining,
  resetCase,
  supersededSenderInert,
  restoreOwnershipToResetIdentity,
  successorAdmins,
  successorMissingPeople,
  successorPeople,
  type PersonLeaf,
} from "./reset-repair";

const OLD = "old";
const NEW = "new";
const ME = "me";
const BOB = "bob";

describe("resetCase (R.7)", () => {
  it("a one-to-one is never repaired", () => {
    expect(resetCase({ kind: "chat", ownerKeyB64: ME, supersededKeyB64: OLD })).toBe("one-to-one");
    expect(resetCase({ kind: "chat", ownerKeyB64: undefined, supersededKeyB64: OLD })).toBe("one-to-one");
  });
  it("a group with no role table is left alone", () => {
    expect(resetCase({ kind: "group", ownerKeyB64: undefined, supersededKeyB64: OLD })).toBe("no-role-table");
  });
  it("a group the superseded identity owns continues in a successor", () => {
    expect(resetCase({ kind: "group", ownerKeyB64: OLD, supersededKeyB64: OLD })).toBe("owned");
  });
  it("any other role-table group is repaired in place", () => {
    expect(resetCase({ kind: "group", ownerKeyB64: ME, supersededKeyB64: OLD })).toBe("repair");
  });
});

const ROSTER: PersonLeaf[] = [
  { leafIndex: 0, personKeyB64: ME },
  { leafIndex: 1, personKeyB64: OLD },
  { leafIndex: 2, personKeyB64: BOB },
  { leafIndex: 3, personKeyB64: OLD },
];

describe("repairPlan (R.7 case 2)", () => {
  const base = {
    roster: ROSTER,
    supersededKeyB64: OLD,
    pendingRemoveLeaves: [] as number[],
    removeInFlight: false,
    membershipEnded: false,
  };
  it("an owner or admin proposes the removal of one old leaf and may add", () => {
    for (const ownRole of ["owner", "admin"] as const) {
      expect(repairPlan({ ...base, ownRole })).toEqual({ removeLeaf: 1, mayAdd: true });
    }
  });
  it("skips a leaf whose removal is already pending, and takes the next", () => {
    expect(repairPlan({ ...base, ownRole: "admin", pendingRemoveLeaves: [1] })).toEqual({
      removeLeaf: 3,
      mayAdd: true,
    });
    expect(repairPlan({ ...base, ownRole: "admin", pendingRemoveLeaves: [1, 3] })).toEqual({
      removeLeaf: null,
      mayAdd: true,
    });
  });
  it("does not propose twice while its own removal is in flight", () => {
    expect(repairPlan({ ...base, ownRole: "owner", removeInFlight: true }).removeLeaf).toBeNull();
  });
  it("a plain member, an unreadable role, an ended membership and a torn roster propose nothing", () => {
    const none = { removeLeaf: null, mayAdd: false };
    expect(repairPlan({ ...base, ownRole: "member" })).toEqual(none);
    expect(repairPlan({ ...base, ownRole: null })).toEqual(none);
    expect(repairPlan({ ...base, ownRole: "owner", membershipEnded: true })).toEqual(none);
    expect(repairPlan({ ...base, ownRole: "owner", roster: undefined })).toEqual(none);
  });
  it("once the old leaves are gone only the add remains", () => {
    const roster = ROSTER.filter((l) => l.personKeyB64 !== OLD);
    expect(repairPlan({ ...base, ownRole: "admin", roster })).toEqual({ removeLeaf: null, mayAdd: true });
    expect(leavesOfKey(roster, OLD)).toEqual([]);
  });
});

describe("mayAddResetIdentity", () => {
  it("only an owner or an admin proposes the new identity's devices", () => {
    expect(mayAddResetIdentity("owner")).toBe(true);
    expect(mayAddResetIdentity("admin")).toBe(true);
    expect(mayAddResetIdentity("member")).toBe(false);
    expect(mayAddResetIdentity(null)).toBe(false);
  });
});

describe("adminRestorePlan (R.7 case 2)", () => {
  const base = {
    adminKeysB64: [OLD, BOB],
    supersededKeyB64: OLD,
    newKeyB64: NEW,
    rosterPersonKeysB64: [ME, BOB, NEW],
  };
  it("the owner puts the new key in the superseded admin's place", () => {
    expect(adminRestorePlan({ ...base, ownRole: "owner" })).toEqual({ adminKeysB64: [BOB, NEW] });
  });
  it("nobody but the owner proposes a role change", () => {
    expect(adminRestorePlan({ ...base, ownRole: "admin" })).toBeNull();
    expect(adminRestorePlan({ ...base, ownRole: "member" })).toBeNull();
  });
  it("nothing when the superseded key was not an admin", () => {
    expect(adminRestorePlan({ ...base, ownRole: "owner", adminKeysB64: [BOB] })).toBeNull();
  });
  it("waits until the new identity is a member: no role for a key no leaf holds", () => {
    expect(adminRestorePlan({ ...base, ownRole: "owner", rosterPersonKeysB64: [ME, BOB] })).toBeNull();
  });
  it("does not list the new key twice", () => {
    expect(adminRestorePlan({ ...base, ownRole: "owner", adminKeysB64: [OLD, NEW] })).toEqual({
      adminKeysB64: [NEW],
    });
  });
});

describe("membersChangedSuppressed (UX 16.5)", () => {
  const account = (k: string): string | null => (k === OLD || k === NEW ? "acct-anna" : null);
  it("a swap of one person's devices leaves the set of people unchanged", () => {
    const before = peopleByAccount([ME, OLD, BOB], account);
    const after = peopleByAccount([ME, BOB, NEW], account);
    expect(membersChangedSuppressed(before, after)).toBe(true);
  });
  it("the removal alone (the add not landed yet) is a change of people", () => {
    const before = peopleByAccount([ME, OLD, BOB], account);
    const after = peopleByAccount([ME, BOB], account);
    expect(membersChangedSuppressed(before, after)).toBe(false);
  });
  it("a real join or departure is never suppressed", () => {
    expect(membersChangedSuppressed([ME, BOB], [ME, BOB, "carol"])).toBe(false);
    expect(membersChangedSuppressed([ME, BOB], [ME])).toBe(false);
    expect(membersChangedSuppressed([ME, BOB], [ME, "carol"])).toBe(false);
  });
  it("a torn read suppresses nothing", () => {
    expect(membersChangedSuppressed(null, [ME])).toBe(false);
    expect(membersChangedSuppressed([ME], null)).toBe(false);
  });
});

describe("the two decisions awaiting confirmation", () => {
  it("default: ownership goes back to the identity that reset", () => {
    expect(restoreOwnershipToResetIdentity()).toBe(true);
  });
  it("default: the founder re-sends its own messages sent after the reset was signed", () => {
    expect(resendOwnMessagesAfterReset()).toBe(true);
  });
});

describe("messagesToResend (R.7 case 1, D5)", () => {
  const SIGNED = 1000;
  it("own, resendable, strictly after the reset was signed, in order", () => {
    expect(
      messagesToResend(
        [
          { messageId: 9, own: true, sentAtMs: 1500, resendable: true },
          { messageId: 4, own: true, sentAtMs: 1001, resendable: true },
          { messageId: 5, own: false, sentAtMs: 1200, resendable: true },
          { messageId: 6, own: true, sentAtMs: 1000, resendable: true },
          { messageId: 7, own: true, sentAtMs: 900, resendable: true },
          { messageId: 8, own: true, sentAtMs: 1300, resendable: false },
          { messageId: 10, own: true, sentAtMs: null, resendable: true },
        ],
        SIGNED,
      ),
    ).toEqual([4, 9]);
  });
});

describe("the owned group's successor (R.7 case 3)", () => {
  const superseded = new Set([OLD]);
  it("everyone of the old roster but the superseded keys and the founder", () => {
    expect(
      successorPeople({
        oldRosterPersonKeysB64: [OLD, ME, BOB, "carol", BOB],
        supersededKeysB64: superseded,
        ownPersonKeyB64: ME,
      }),
    ).toEqual([BOB, "carol"]);
  });
  it("the completeness re-check names who is still missing", () => {
    expect(
      successorMissingPeople({
        oldRosterPersonKeysB64: [OLD, ME, BOB, "carol"],
        successorPersonKeysB64: [ME, BOB, NEW],
        supersededKeysB64: superseded,
      }),
    ).toEqual(["carol"]);
  });
  it("restores the admins that moved, never the owner slot or a superseded key", () => {
    expect(
      successorAdmins({
        oldAdminKeysB64: [ME, BOB, "carol", OLD],
        successorPersonKeysB64: [ME, BOB, NEW],
        successorOwnerKeyB64: ME,
        supersededKeysB64: superseded,
      }),
    ).toEqual([BOB]);
  });
});

describe("dropsSupersededSender (R.5 / R.8)", () => {
  it("drops a row from a key an accepted reset retired, and nothing else", () => {
    const superseded = new Map([[OLD, "acct-anna"]]);
    expect(dropsSupersededSender(OLD, superseded)).toBe(true);
    expect(dropsSupersededSender(NEW, superseded)).toBe(false);
    expect(dropsSupersededSender(undefined, superseded)).toBe(false);
  });
});

describe("adminRestoreStep (one role change per sweep)", () => {
  const base = {
    ownRole: "owner" as const,
    adminKeysB64: [OLD, BOB],
    supersededKeyB64: OLD,
    newKeyB64: NEW,
    rosterPersonKeysB64: [ME, BOB, NEW],
    roleChangePending: false,
  };
  it("promotes the new key first, then demotes the old one", () => {
    expect(adminRestoreStep(base)).toEqual({ kind: "promote", keyB64: NEW });
    expect(adminRestoreStep({ ...base, adminKeysB64: [OLD, BOB, NEW] })).toEqual({
      kind: "demote",
      keyB64: OLD,
    });
    expect(adminRestoreStep({ ...base, adminKeysB64: [BOB, NEW] })).toBeNull();
  });
  it("waits behind a pending role change, and for the new identity to be a member", () => {
    expect(adminRestoreStep({ ...base, roleChangePending: true })).toBeNull();
    expect(adminRestoreStep({ ...base, rosterPersonKeysB64: [ME, BOB] })).toBeNull();
  });
  it("is the owner's alone", () => {
    expect(adminRestoreStep({ ...base, ownRole: "admin" })).toBeNull();
  });
});

describe("deviceSwapAcrossCommits (UX 16.5, the Remove and the Add in two commits)", () => {
  const account = (k: string): string | null =>
    k === "old" || k === "new" ? "acct-anna" : k === "bob" ? "acct-bob" : null;
  const isSuperseded = (k: string): boolean => k === "old";
  const judge = (
    before: readonly string[] | null,
    after: readonly string[] | null,
    awaited: Record<string, number> = {},
    nowMs = 1_000,
  ) =>
    deviceSwapAcrossCommits({
      personKeysBefore: before,
      personKeysAfter: after,
      accountOfKey: account,
      isSuperseded,
      awaited,
      nowMs,
    });

  it("one batch carrying both halves is suppressed, with no memory", () => {
    expect(judge(["me", "old", "bob"], ["me", "new", "bob"])).toEqual({ suppress: true, awaited: {} });
  });
  it("the Remove of the old leaves alone is suppressed and remembered", () => {
    expect(judge(["me", "old", "bob"], ["me", "bob"])).toEqual({
      suppress: true,
      awaited: { "acct-anna": 1_000 },
    });
  });
  it("the Add of the new devices that follows is suppressed and forgotten", () => {
    expect(judge(["me", "bob"], ["me", "bob", "new"], { "acct-anna": 1_000 }, 2_000)).toEqual({
      suppress: true,
      awaited: {},
    });
  });
  it("the Add first and the Remove second change no person either time", () => {
    expect(judge(["me", "old"], ["me", "old", "new"]).suppress).toBe(true);
    expect(judge(["me", "old", "new"], ["me", "new"]).suppress).toBe(true);
  });
  it("a person removed while they hold a current key keeps the row", () => {
    expect(judge(["me", "bob"], ["me"])).toEqual({ suppress: false, awaited: {} });
    expect(judge(["me", "new"], ["me"]).suppress).toBe(false);
  });
  it("a person nobody was waiting for keeps the row", () => {
    expect(judge(["me"], ["me", "new"]).suppress).toBe(false);
    expect(judge(["me"], ["me", "carol"], { "acct-anna": 1_000 }).suppress).toBe(false);
  });
  it("a commit mixing the awaited return with a stranger keeps the row and clears the wait", () => {
    expect(judge(["me"], ["me", "new", "carol"], { "acct-anna": 1_000 }, 2_000)).toEqual({
      suppress: false,
      awaited: {},
    });
  });
  it("a removal mixing a superseded person with a current one keeps the row", () => {
    expect(judge(["me", "old", "bob"], ["me"]).suppress).toBe(false);
  });
  it("a wait older than the window is forgotten before judging", () => {
    const late = 1_000 + DEVICE_SWAP_PAIR_WINDOW_MS;
    expect(judge(["me"], ["me", "new"], { "acct-anna": 1_000 }, late)).toEqual({
      suppress: false,
      awaited: {},
    });
  });
  it("a commit that changes no person key (a device admitted or retired) is suppressed and keeps the memory", () => {
    expect(judge(["me", "bob"], ["me", "bob"], { "acct-anna": 1_000 }, 2_000)).toEqual({
      suppress: true,
      awaited: { "acct-anna": 1_000 },
    });
    // A one-to-one where this account's new device was admitted.
    expect(judge(["me", "bob"], ["bob", "me"]).suppress).toBe(true);
  });
  it("a person who joins or leaves still keeps the row", () => {
    expect(judge(["me", "bob"], ["me", "bob", "carol"]).suppress).toBe(false);
    expect(judge(["me", "bob", "carol"], ["me", "bob"]).suppress).toBe(false);
  });
  it("a torn read suppresses nothing and keeps the memory", () => {
    expect(judge(["me", "bob"], null, { "acct-anna": 1_000 }, 2_000)).toEqual({
      suppress: false,
      awaited: { "acct-anna": 1_000 },
    });
    expect(judge(null, ["me"], { "acct-anna": 1_000 }, 2_000)).toEqual({
      suppress: false,
      awaited: { "acct-anna": 1_000 },
    });
  });
});

describe("resendRemaining (R.7 case 1: an interrupted re-send resumes)", () => {
  it("with no cursor everything is still to send, in order", () => {
    expect(resendRemaining([3, 5, 8], null)).toEqual([3, 5, 8]);
  });
  it("continues after the last message that landed, never from the first", () => {
    expect(resendRemaining([3, 5, 8], 3)).toEqual([5, 8]);
    expect(resendRemaining([3, 5, 8], 5)).toEqual([8]);
  });
  it("a finished re-send has nothing left", () => {
    expect(resendRemaining([3, 5, 8], 8)).toEqual([]);
  });
  it("a cursor between two ids skips only what is at or below it", () => {
    expect(resendRemaining([3, 5, 8], 6)).toEqual([8]);
  });
});

describe("resendParts (R.7 case 1: a re-send is faithful)", () => {
  it("a reply keeps its quote and carries target 0", () => {
    expect(resendParts({ reply: { target: 17, author: "Ada", excerpt: "nine" } })).toEqual({
      reply: { target: RESEND_REPLY_TARGET, author: "Ada", excerpt: "nine" },
    });
    expect(RESEND_REPLY_TARGET).toBe(0);
  });
  it("a forward keeps its attribution and origin untouched", () => {
    const forward = { author: "Grace", origin: { groupIdB64: "Zw==", messageId: 9 } };
    expect(resendParts({ forward })).toEqual({ forward });
  });
  it("a plain message carries nothing more", () => {
    expect(resendParts({})).toEqual({});
    expect(resendParts({ reply: undefined, forward: undefined })).toEqual({});
  });
});

describe("supersededSenderInert (R.8: nothing from a superseded sender acts)", () => {
  const superseded = new Set(["retired"]);
  const base = { supersededKeys: superseded, moved: false } as const;
  it("a sender whose key no reset retired is never inert", () => {
    expect(
      supersededSenderInert({ ...base, senderKeyB64: "current", kind: "chat", ownerKeyB64: undefined, moved: true }),
    ).toBe(false);
    expect(
      supersededSenderInert({ ...base, senderKeyB64: undefined, kind: "chat", ownerKeyB64: undefined }),
    ).toBe(false);
  });
  it("a one-to-one cannot be repaired in place: the retired key is inert", () => {
    expect(supersededSenderInert({ ...base, senderKeyB64: "retired", kind: "chat", ownerKeyB64: undefined })).toBe(true);
  });
  it("a group the retired key owned, and a group with no role table, likewise", () => {
    expect(supersededSenderInert({ ...base, senderKeyB64: "retired", kind: "group", ownerKeyB64: "retired" })).toBe(true);
    expect(supersededSenderInert({ ...base, senderKeyB64: "retired", kind: "group", ownerKeyB64: undefined })).toBe(true);
  });
  it("a group repaired in place still applies its service envelopes until the eviction lands", () => {
    expect(supersededSenderInert({ ...base, senderKeyB64: "retired", kind: "group", ownerKeyB64: "owner" })).toBe(false);
  });
  it("a conversation that already moved is inert for the retired key whatever its case", () => {
    expect(
      supersededSenderInert({ ...base, senderKeyB64: "retired", kind: "group", ownerKeyB64: "owner", moved: true }),
    ).toBe(true);
  });
});
