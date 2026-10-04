// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 phase 2 (UX 16.5) — the Recreate group sheet's rows: pre-filled
 * with every current member except the old owner, the reset person's new
 * identity unticked and disabled while unverified, the submit gate.
 */
import { describe, expect, it } from "vitest";

import {
  initialTicks,
  recreateInvitees,
  recreateRows,
  recreateSubmitEnabled,
  type RecreateMember,
} from "./recreate-group";

const roster: RecreateMember[] = [
  { personKeyB64: "me", label: "Me", accountIdHex: "aa".repeat(16), signaturePubkeyB64: "me" },
  { personKeyB64: "old", label: "Anna", accountIdHex: "bb".repeat(16), signaturePubkeyB64: "old" },
  { personKeyB64: "ivan", label: "Ivan", accountIdHex: "cc".repeat(16), signaturePubkeyB64: "ivan" },
  { personKeyB64: "stranger", label: "Member", accountIdHex: undefined, signaturePubkeyB64: undefined },
];

describe("recreateRows", () => {
  it("lists every member but me and the old owner, ticked when addable, dimmed otherwise", () => {
    const rows = recreateRows({ roster, ownPersonKeyB64: "me", oldOwnerKeyB64: "old", resetContact: null });
    expect(rows.map((r) => [r.personKeyB64, r.checked, r.enabled, r.reason])).toEqual([
      ["ivan", true, true, null],
      ["stranger", false, false, "no-account-id"],
    ]);
  });

  it("the new identity is unticked and disabled with the reason while unverified", () => {
    const rows = recreateRows({
      roster,
      ownPersonKeyB64: "me",
      oldOwnerKeyB64: "old",
      resetContact: { label: "Anna", accountIdHex: "bb".repeat(16), signaturePubkeyB64: "new", unverified: true },
    });
    const anna = rows.find((r) => r.personKeyB64 === "new");
    expect(anna).toEqual({
      personKeyB64: "new",
      label: "Anna",
      accountIdHex: "bb".repeat(16),
      signaturePubkeyB64: "new",
      checked: false,
      enabled: false,
      reason: "keys-reset-unverified",
    });
  });

  it("ticked and enabled once verified, and not listed twice when already in the roster", () => {
    const rows = recreateRows({
      roster: [...roster, { personKeyB64: "new", label: "Anna", accountIdHex: "bb".repeat(16), signaturePubkeyB64: "new" }],
      ownPersonKeyB64: "me",
      oldOwnerKeyB64: "old",
      resetContact: { label: "Anna", accountIdHex: "bb".repeat(16), signaturePubkeyB64: "new", unverified: false },
    });
    expect(rows.filter((r) => r.personKeyB64 === "new")).toHaveLength(1);
    expect(rows.find((r) => r.personKeyB64 === "new")).toMatchObject({ checked: true, enabled: true, reason: null });
  });
});

describe("the submit", () => {
  const rows = recreateRows({
    roster,
    ownPersonKeyB64: "me",
    oldOwnerKeyB64: "old",
    resetContact: { label: "Anna", accountIdHex: "bb".repeat(16), signaturePubkeyB64: "new", unverified: true },
  });

  it("is disabled when no member is ticked, and a disabled row can never be invited", () => {
    expect(recreateSubmitEnabled(rows, new Set())).toBe(false);
    expect(recreateSubmitEnabled(rows, new Set(["new", "stranger"]))).toBe(false);
    expect(recreateInvitees(rows, new Set(["new", "stranger"]))).toEqual([]);
  });

  it("invites the ticked addable rows in list order, the pre-set ticks by default", () => {
    expect(initialTicks(rows)).toEqual(new Set(["ivan"]));
    expect(recreateInvitees(rows, initialTicks(rows))).toEqual([
      { accountIdHex: "cc".repeat(16), signaturePubkeyB64: "ivan", label: "Ivan" },
    ]);
    expect(recreateSubmitEnabled(rows, initialTicks(rows))).toBe(true);
  });
});
