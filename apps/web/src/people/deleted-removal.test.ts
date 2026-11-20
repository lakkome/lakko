// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0098 section 5 — the removal plan, and that only a statement leads to it. */
import { describe, expect, it } from "vitest";

import { withAnswer, withRemoval, withStatement } from "./account-status";
import { deletedRemovalPlan, type RemovalFacts } from "./deleted-removal";

const base: RemovalFacts = {
  kind: "group",
  ownRole: "admin",
  ownerKeyB64: "OWNER",
  deletedKeyB64: "GONE",
  roster: [
    { leafIndex: 0, personKeyB64: "OWNER" },
    { leafIndex: 1, personKeyB64: "ME" },
    { leafIndex: 2, personKeyB64: "GONE" },
    { leafIndex: 3, personKeyB64: "GONE" },
  ],
  pendingRemoveLeaves: [],
  alreadyProposed: false,
};

describe("removing a deleted account's leaves from a group", () => {
  it("an admin or the owner proposes, naming one of the leaves", () => {
    expect(deletedRemovalPlan(base)).toEqual({ kind: "propose", leafIndex: 2 });
    expect(deletedRemovalPlan({ ...base, ownRole: "owner" })).toEqual({ kind: "propose", leafIndex: 2 });
  });

  it("a plain member does not", () => {
    expect(deletedRemovalPlan({ ...base, ownRole: "member" })).toEqual({ kind: "none" });
    expect(deletedRemovalPlan({ ...base, ownRole: null })).toEqual({ kind: "none" });
  });

  it("a one-to-one is left as it is", () => {
    expect(deletedRemovalPlan({ ...base, kind: "chat" })).toEqual({ kind: "none" });
  });

  it("the owner's leaves are never removed (C-63)", () => {
    expect(deletedRemovalPlan({ ...base, ownerKeyB64: "GONE" })).toEqual({ kind: "none" });
  });

  it("once per group: not again after it was sent, nor while a remove is pending", () => {
    expect(deletedRemovalPlan({ ...base, alreadyProposed: true })).toEqual({ kind: "none" });
    expect(deletedRemovalPlan({ ...base, pendingRemoveLeaves: [2, 3] })).toEqual({ kind: "none" });
  });

  it("a person no longer in the group is done", () => {
    expect(deletedRemovalPlan({ ...base, roster: base.roster.slice(0, 2) })).toEqual({ kind: "gone" });
  });

  it("only the statement records an owed removal; a directory Absent records none", () => {
    // The runtime records `owed` from the statement sink alone; the answer
    // path never writes removals.
    const fromAbsent = withAnswer(undefined, "absent", 1);
    expect(fromAbsent.removals).toBeUndefined();
    const fromStatement = withRemoval(withStatement(undefined), "g", "owed");
    expect(fromStatement.removals).toEqual({ g: "owed" });
  });
});
