// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 — ownership transfer, the owner's way out
 * ([`docs/ux/proposal-based-membership.md`] §6.1).
 *
 * `routes/Chat.tsx` cannot be tested at all — `vitest.config.ts` collects
 * `src/**\/*.test.ts` in a plain `node` environment with no jsdom and no Solid
 * JSX transform — so every rule §6.1 states lives in `ownership-transfer.ts`
 * and is pinned here, exactly as `pending-requests.ts` does for §5.
 *
 * ## Which of these discriminate (MEASURED — each probe was run and reverted)
 *
 * Recorded in the report accompanying this change; the summary is that each
 * `it` below was re-run against a deliberately broken rule and observed to go
 * red, except the ones marked BOUNDARY GUARD, whose job is to fail when the
 * §6.1 copy drifts rather than when a rule changes.
 */

import { afterEach, beforeEach, describe, expect, it } from "vitest";

/** The `localStorage` shim convention this repo already uses
 *  (`chat/pending-requests.test.ts`). */
class MemoryStorage {
  private readonly map = new Map<string, string>();
  get length(): number {
    return this.map.size;
  }
  key(i: number): string | null {
    return [...this.map.keys()][i] ?? null;
  }
  getItem(k: string): string | null {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, v);
  }
  removeItem(k: string): void {
    this.map.delete(k);
  }
  clear(): void {
    this.map.clear();
  }
}

beforeEach(() => {
  (globalThis as unknown as { localStorage: unknown }).localStorage =
    new MemoryStorage();
});

afterEach(() => {
  delete (globalThis as unknown as { localStorage?: unknown }).localStorage;
});

import {
  LEAVE_BLOCKED_BY_OWNERSHIP,
  NEW_OWNER_BODY,
  NEW_OWNER_DISMISS,
  NO_TRANSFER_CANDIDATES,
  OUTGOING_OWNER_OPTIONS,
  PRESELECTED_OUTGOING_OWNER,
  ROLE_CHANGE_ALREADY_PENDING,
  TRANSFER_IRREVERSIBLE_BODY,
  TRANSFER_IRREVERSIBLE_HEADLINE,
  TRANSFER_UNLOCKS_LEAVE,
  clearLastObservedOwnerForAccount,
  exitBlock,
  loadLastObservedOwner,
  newOwnerNotice,
  rememberObservedOwner,
  transferActionLabel,
  transferCandidates,
  transferOutcomeNotice,
  transferPendingLabel,
  type ExitBlockInput,
  type RosterEntryView,
} from "./ownership-transfer";

const ACCOUNT = "acct-a";
const GROUP = "grp-1";

const OWNER_KEY = "b3duZXItLS0tLS0tLS0tLS0tLS0tLS0tLS0=";
const ALICE_KEY = "YWxpY2UtLS0tLS0tLS0tLS0tLS0tLS0tLS0=";
const BOB_KEY = "Ym9iLS0tLS0tLS0tLS0tLS0tLS0tLS0tLS0=";

function roster(): RosterEntryView[] {
  return [
    { leafIndex: 0, signatureKeyB64: OWNER_KEY, label: "Owner" },
    { leafIndex: 1, signatureKeyB64: ALICE_KEY, label: "Alice" },
    { leafIndex: 2, signatureKeyB64: BOB_KEY, label: "b0b1c2d3" },
  ];
}

function exitInput(over: Partial<ExitBlockInput> = {}): ExitBlockInput {
  return {
    usesProposalPath: true,
    isOwner: false,
    ownLeafKnown: true,
    candidateCount: 2,
    waitingRoleChange: null,
    ownRemovalPending: false,
    // ADR-0045 UX §6.2.7 — the third exit-block state. These cases are all
    // about a reader who is still IN the group; the ended arm is pinned in
    // `membership-ended.test.ts`, beside the state that produces it.
    membershipEnded: false,
    ...over,
  };
}

describe("§6.1.1 — the exit block, two role-disjoint controls", () => {
  it("the owner sees Transfer ownership live and Leave group disabled with its reason", () => {
    const block = exitBlock(exitInput({ isOwner: true }));
    expect(block.kind).toBe("owner");
    if (block.kind !== "owner") return;
    expect(block.transfer.enabled).toBe(true);
    expect(block.transfer.disabledReason).toBeNull();
    // The reversal §6.1.1 argues for: present and disabled, never hidden. An
    // owner looking for the exit must not find nothing at all about leaving.
    expect(block.leave.enabled).toBe(false);
    expect(block.leave.disabledReason).toBe(LEAVE_BLOCKED_BY_OWNERSHIP);
    expect(block.leave.disabledReason).toBe(
      "Transfer ownership first — a group cannot be left without an owner.",
    );
  });

  it("a non-owner sees Leave group alone, live, and NO transfer control", () => {
    const block = exitBlock(exitInput({ isOwner: false }));
    expect(block.kind).toBe("member");
    if (block.kind !== "member") return;
    expect(block.leave.enabled).toBe(true);
    expect(block.leave.disabledReason).toBeNull();
    // `kind: "member"` carries no `transfer` field at all — a non-owner's
    // transfer is refused by the core and by every peer, so the control would
    // never work and nothing a member could do would make it work. That is the
    // case where "a control that always refuses is worse than no control" still
    // holds.
    expect("transfer" in block).toBe(false);
  });

  it("a group with no role table shows no exit block at all", () => {
    // No role table means no owner, so there is nobody to transfer to and no
    // self-remove route in the core either.
    expect(exitBlock(exitInput({ usesProposalPath: false })).kind).toBe("hidden");
    expect(
      exitBlock(exitInput({ usesProposalPath: false, isOwner: true })).kind,
    ).toBe("hidden");
  });

  it("an unresolvable own leaf shows no exit block", () => {
    expect(exitBlock(exitInput({ ownLeafKnown: false })).kind).toBe("hidden");
  });

  it("Leave group does not turn live one instant early", () => {
    // §6.1.7: it stays disabled for the transferor until the role table
    // actually names someone else — read off the role table, NEVER off the
    // local record of having asked. An owner is still the owner while their
    // transfer is pending, and a `Leave group` that went live on publication
    // would offer an action the core refuses and the cryptography does not
    // support. So the gate is `isOwner` and a pending own transfer changes
    // nothing about it.
    const pending = exitBlock(
      exitInput({
        isOwner: true,
        waitingRoleChange: { ownTransfer: true, successorLabel: "Alice" },
      }),
    );
    expect(pending.kind).toBe("owner");
    if (pending.kind !== "owner") return;
    expect(pending.leave.enabled).toBe(false);

    // …and the moment the table names someone else, the same control in the
    // same slot is simply live. Nothing has to announce the transition.
    const landed = exitBlock(
      exitInput({
        isOwner: false,
        waitingRoleChange: { ownTransfer: true, successorLabel: "Alice" },
      }),
    );
    expect(landed.kind).toBe("member");
    if (landed.kind !== "member") return;
    expect(landed.leave.enabled).toBe(true);
  });

  it("a member with their own leave pending keeps Leave visible but disabled", () => {
    const block = exitBlock(exitInput({ ownRemovalPending: true }));
    expect(block.kind).toBe("member");
    if (block.kind !== "member") return;
    expect(block.leave.enabled).toBe(false);
    // §5.2's existing annotation, reused — not new copy.
    expect(block.leave.disabledReason).toBe("Waiting to leave.");
  });
});

describe("§6.1.3 / §6.1.6 / §6.1.8 — why Transfer ownership is disabled", () => {
  it("a group with no other member disables transfer with its own terminal reason", () => {
    const block = exitBlock(exitInput({ isOwner: true, candidateCount: 0 }));
    expect(block.kind).toBe("owner");
    if (block.kind !== "owner") return;
    expect(block.transfer.enabled).toBe(false);
    expect(block.transfer.disabledReason).toBe(NO_TRANSFER_CANDIDATES);
    expect(block.transfer.disabledReason).toBe(
      "There is no one else in this group to transfer it to.",
    );
  });

  it("a waiting role change disables transfer with §6's SEVENTH string", () => {
    const block = exitBlock(
      exitInput({
        isOwner: true,
        waitingRoleChange: { ownTransfer: false, successorLabel: null },
      }),
    );
    expect(block.kind).toBe("owner");
    if (block.kind !== "owner") return;
    expect(block.transfer.enabled).toBe(false);
    // §6.1.8: the table stays at seven strings. This is the seventh, verbatim,
    // and it is the ONE availability refusal in the table — the only one that
    // ends by telling the user to try again.
    expect(block.transfer.disabledReason).toBe(ROLE_CHANGE_ALREADY_PENDING);
    expect(block.transfer.disabledReason).toBe(
      "A role change is already waiting to apply. Wait for it to land, then try again.",
    );
  });

  it("our OWN waiting transfer names the successor instead", () => {
    const block = exitBlock(
      exitInput({
        isOwner: true,
        waitingRoleChange: { ownTransfer: true, successorLabel: "Alice" },
      }),
    );
    expect(block.kind).toBe("owner");
    if (block.kind !== "owner") return;
    expect(block.transfer.enabled).toBe(false);
    expect(block.transfer.disabledReason).toBe("Waiting to make Alice the owner.");
  });

  it("our own waiting transfer whose successor no longer resolves falls back", () => {
    const block = exitBlock(
      exitInput({
        isOwner: true,
        waitingRoleChange: { ownTransfer: true, successorLabel: null },
      }),
    );
    expect(block.kind).toBe("owner");
    if (block.kind !== "owner") return;
    expect(block.transfer.disabledReason).toBe("Ownership transfer waiting to apply.");
  });

  it("our own PROMOTE is not our own transfer, and gets the seventh string", () => {
    // `ProposalKind::RoleChange` carries no payload, so the successor key on
    // our own observation record is the only thing that tells a transfer from a
    // promote. A promote reporting itself as `Ownership transfer waiting to
    // apply.` would be a plain falsehood; the seventh string is true of it.
    const block = exitBlock(
      exitInput({
        isOwner: true,
        waitingRoleChange: { ownTransfer: false, successorLabel: null },
      }),
    );
    expect(block.kind).toBe("owner");
    if (block.kind !== "owner") return;
    expect(block.transfer.disabledReason).toBe(ROLE_CHANGE_ALREADY_PENDING);
  });

  it("a PERMANENT refusal outranks the retry-inviting one", () => {
    // §6 draws the distinction and it decides the precedence: an authorisation
    // refusal is a statement of fact with no invitation to retry, an
    // availability refusal ends by telling the user to try again. An empty
    // roster is permanent, so it must not be masked by a string that promises a
    // retry which can never succeed.
    const block = exitBlock(
      exitInput({
        isOwner: true,
        candidateCount: 0,
        waitingRoleChange: { ownTransfer: false, successorLabel: null },
      }),
    );
    expect(block.kind).toBe("owner");
    if (block.kind !== "owner") return;
    expect(block.transfer.disabledReason).toBe(NO_TRANSFER_CANDIDATES);
  });

  it("our own request outranks even the permanent one — §10's own-request exception", () => {
    const block = exitBlock(
      exitInput({
        isOwner: true,
        candidateCount: 0,
        waitingRoleChange: { ownTransfer: true, successorLabel: "Alice" },
      }),
    );
    expect(block.kind).toBe("owner");
    if (block.kind !== "owner") return;
    expect(block.transfer.disabledReason).toBe("Waiting to make Alice the owner.");
  });

  it("a role change INSIDE the quiet window leaves the control enabled", () => {
    // §6.1.6: below the quiet window the control stays enabled and a second tap
    // is refused. `waitingRoleChange` is null for a request that has not
    // matured, so this is the caller's `state === \"waiting\"` filter arriving
    // here as an absence. A request not yet rendered must not silently disable
    // a control with nothing to explain it.
    const block = exitBlock(exitInput({ isOwner: true, waitingRoleChange: null }));
    expect(block.kind).toBe("owner");
    if (block.kind !== "owner") return;
    expect(block.transfer.enabled).toBe(true);
  });
});

describe("§6.1.3 — the picker offers every member, marked not filtered", () => {
  it("excludes the current owner and includes a non-admin member", () => {
    const rows = transferCandidates(roster(), OWNER_KEY, new Set([ALICE_KEY]));
    expect(rows.map((r) => r.signatureKeyB64)).toEqual([ALICE_KEY, BOB_KEY]);
    // The reason this is not an admins-only picker is a dead end rather than a
    // preference: in the commonest small group — owner plus one member, no
    // admins ever appointed — an admins-only picker is EMPTY and the owner is
    // stuck again, which is the exact dead end this feature exists to close.
    expect(rows.find((r) => r.signatureKeyB64 === BOB_KEY)?.isAdmin).toBe(false);
  });

  it("marks a current admin rather than sorting or filtering them", () => {
    const rows = transferCandidates(roster(), OWNER_KEY, new Set([BOB_KEY]));
    expect(rows.map((r) => r.isAdmin)).toEqual([false, true]);
    // Roster order, unchanged. Sorting admins to the top would be a soft
    // version of the filter §6.1.3 rejects.
    expect(rows.map((r) => r.label)).toEqual(["Alice", "b0b1c2d3"]);
  });

  it("keyed on the signature key, never the leaf index", () => {
    const rows = transferCandidates(roster(), OWNER_KEY, new Set());
    // OpenMLS hands a vacated leaf to the next Add, so a leaf-keyed selection
    // can silently come to name someone else. The selection VALUE is the key.
    for (const row of rows) expect(typeof row.signatureKeyB64).toBe("string");
    expect(rows.map((r) => r.leafIndex)).toEqual([1, 2]);
  });

  it("a one-person group yields no candidates", () => {
    const rows = transferCandidates(
      [{ leafIndex: 0, signatureKeyB64: OWNER_KEY, label: "Owner" }],
      OWNER_KEY,
      new Set(),
    );
    expect(rows).toHaveLength(0);
  });

  it("reuses the roster's own label verbatim", () => {
    // The label the user reads in the picker and the label they read on the
    // roster must be produced by the same function, because whom they are
    // naming is the one thing they will cross-check.
    const rows = transferCandidates(roster(), OWNER_KEY, new Set());
    expect(rows[1]?.label).toBe("b0b1c2d3");
  });
});

describe("§6.1.4 — the outgoing-owner choice", () => {
  it("`Stay an admin` is pre-selected", () => {
    expect(PRESELECTED_OUTGOING_OWNER).toBe("admin");
    const preselected = OUTGOING_OWNER_OPTIONS.filter((o) => o.preSelected);
    expect(preselected).toHaveLength(1);
    expect(preselected[0]?.value).toBe("admin");
    expect(preselected[0]?.label).toBe("Stay an admin");
  });

  it("each option carries its own consequence string", () => {
    expect(OUTGOING_OWNER_OPTIONS.map((o) => o.value)).toEqual(["admin", "member"]);
    expect(OUTGOING_OWNER_OPTIONS[0]?.consequence).toBe(
      "You can still add and remove members. You can give this up later by asking the new owner.",
    );
    expect(OUTGOING_OWNER_OPTIONS[1]?.label).toBe("Become a regular member");
    expect(OUTGOING_OWNER_OPTIONS[1]?.consequence).toBe(
      "You give up every group power immediately. Only the new owner can give any of it back.",
    );
  });
});

describe("§6.1.5 — the step IS the confirmation", () => {
  it("the action is disabled until a successor is picked", () => {
    const idle = transferActionLabel(undefined);
    expect(idle.enabled).toBe(false);
    // There is no default successor and no path to the action that skips the
    // picker.
    expect(idle.label).toBe("Transfer ownership");
  });

  it("the action names the picked person, using the row's label verbatim", () => {
    const picked = transferActionLabel({
      leafIndex: 2,
      signatureKeyB64: BOB_KEY,
      label: "b0b1c2d3",
      isAdmin: false,
    });
    expect(picked.enabled).toBe(true);
    // Not a re-formatted or re-shortened variant: that is the string the user
    // cross-checks against the row they tapped.
    expect(picked.label).toBe("Make b0b1c2d3 the owner");
  });

  it("the irreversibility copy states three specific losses, not an adjective", () => {
    // BOUNDARY GUARD — this fails when the wording drifts, which is the point:
    // all three clauses are literally true and none is rhetorical.
    expect(TRANSFER_IRREVERSIBLE_HEADLINE).toBe("This cannot be undone.");
    expect(TRANSFER_IRREVERSIBLE_BODY).toBe(
      "Only an owner can change roles. Once you transfer this group you cannot " +
        "take it back, you cannot restore your own owner role, and you cannot " +
        "change your own role again unless the new owner does it for you.",
    );
  });

  it("the trailing line says APPLIED, never NOW", () => {
    expect(TRANSFER_UNLOCKS_LEAVE).toBe(
      "Once the change is applied, you can leave the group like any other member.",
    );
    // Asserting the exit is open before the commit lands would be the
    // optimistic update §3 forbids.
    expect(TRANSFER_UNLOCKS_LEAVE).not.toContain("now");
  });
});

describe("§6.1.7 — what the transferor sees afterwards", () => {
  it("a swept transfer names the transferor's OWN new role", () => {
    // Half the panel changes under them in one tap: an owner who chose
    // `Become a regular member` loses the add form, the invite block, every
    // `Remove` and every admin toggle at once.
    expect(transferOutcomeNotice("committed", "Alice", "admin")).toBe(
      "Alice is now the group owner. You are still an admin.",
    );
    expect(transferOutcomeNotice("committed", "Alice", "member")).toBe(
      "Alice is now the group owner. You are now a regular member.",
    );
  });

  it("a published-but-unswept transfer names NO role", () => {
    // Nothing has changed yet; appending "you are now an admin" to a request
    // that has not landed would be the optimistic-update lie in its purest form.
    const notice = transferOutcomeNotice("proposed", "Alice", "admin");
    expect(notice).toBe(
      "Ownership transfer requested. It applies when an owner or an admin applies the change.",
    );
    expect(notice).not.toContain("admin.");
    expect(transferOutcomeNotice("proposed", "Alice", "member")).toBe(notice);
  });
});

describe("§6.1.6 — the pending label resolves through the live roster", () => {
  it("names the successor when they still resolve", () => {
    expect(transferPendingLabel("Alice")).toBe("Waiting to make Alice the owner.");
  });

  it("falls back when the successor can no longer be resolved", () => {
    expect(transferPendingLabel(null)).toBe("Ownership transfer waiting to apply.");
  });
});

describe("§6.1.9 — telling the new owner, fail-open", () => {
  it("says nothing when no previous owner was ever recorded", () => {
    // Structurally unable to fire on a device that first observes the table
    // already naming it owner — a fresh device, a restore, a first sync. So the
    // rule is fail-open: no remembered previous owner means say nothing.
    expect(newOwnerNotice(undefined, ALICE_KEY, ALICE_KEY, "Owner")).toBeNull();
  });

  it("does not fire on a FIRST observation of an unchanged table", () => {
    expect(newOwnerNotice(ALICE_KEY, ALICE_KEY, ALICE_KEY, "Owner")).toBeNull();
  });

  it("fires, naming the previous owner, when the table changed to name us", () => {
    const notice = newOwnerNotice(OWNER_KEY, ALICE_KEY, ALICE_KEY, "Owner");
    expect(notice).not.toBeNull();
    // Naming them is sound rather than a guess: a role change is owner-only and
    // `owner_survives` (C-63) forbids naming a non-member, so the previous
    // owner did author it.
    expect(notice?.headline).toBe("Owner made you the owner of this group.");
    expect(notice?.body).toBe(NEW_OWNER_BODY);
    expect(notice?.body).toBe(
      "You can add and remove members, change admins, and transfer ownership to someone else.",
    );
    expect(notice?.dismiss).toBe(NEW_OWNER_DISMISS);
  });

  it("omits the name when the previous owner no longer resolves", () => {
    const notice = newOwnerNotice(OWNER_KEY, ALICE_KEY, ALICE_KEY, null);
    expect(notice?.headline).toBe("You are now the owner of this group.");
  });

  it("says nothing to someone who is not the owner", () => {
    // Including the transferor: their last observed owner was themselves and
    // the table now names someone else, which is a change — but not one that
    // makes THEM the owner.
    expect(newOwnerNotice(OWNER_KEY, ALICE_KEY, BOB_KEY, "Owner")).toBeNull();
  });

  it("says nothing when the group has no role table or our key is unknown", () => {
    expect(newOwnerNotice(OWNER_KEY, undefined, ALICE_KEY, "Owner")).toBeNull();
    expect(newOwnerNotice(OWNER_KEY, ALICE_KEY, undefined, "Owner")).toBeNull();
  });
});

describe("§6.1.10 — the last-observed-owner store", () => {
  it("round-trips per (account, group)", () => {
    expect(loadLastObservedOwner(ACCOUNT, GROUP)).toBeUndefined();
    rememberObservedOwner(ACCOUNT, GROUP, OWNER_KEY);
    expect(loadLastObservedOwner(ACCOUNT, GROUP)).toBe(OWNER_KEY);
    expect(loadLastObservedOwner(ACCOUNT, "grp-2")).toBeUndefined();
    expect(loadLastObservedOwner("acct-b", GROUP)).toBeUndefined();
  });

  it("records the CHANGE, so a second read sees the new owner", () => {
    rememberObservedOwner(ACCOUNT, GROUP, OWNER_KEY);
    rememberObservedOwner(ACCOUNT, GROUP, ALICE_KEY);
    expect(loadLastObservedOwner(ACCOUNT, GROUP)).toBe(ALICE_KEY);
  });

  it("is wiped for one account on recover / forgetDevice", () => {
    rememberObservedOwner(ACCOUNT, GROUP, OWNER_KEY);
    rememberObservedOwner("acct-b", GROUP, OWNER_KEY);
    clearLastObservedOwnerForAccount(ACCOUNT);
    expect(loadLastObservedOwner(ACCOUNT, GROUP)).toBeUndefined();
    expect(loadLastObservedOwner("acct-b", GROUP)).toBe(OWNER_KEY);
  });

  it("survives a missing localStorage without throwing", () => {
    delete (globalThis as unknown as { localStorage?: unknown }).localStorage;
    expect(() => rememberObservedOwner(ACCOUNT, GROUP, OWNER_KEY)).not.toThrow();
    expect(loadLastObservedOwner(ACCOUNT, GROUP)).toBeUndefined();
    expect(() => clearLastObservedOwnerForAccount(ACCOUNT)).not.toThrow();
  });
});
