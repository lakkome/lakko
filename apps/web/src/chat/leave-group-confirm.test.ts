// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §6 — the `Leave this group?` confirmation.
 *
 * The copy is §6's, verbatim; what is new is that it lives somewhere a test
 * can read it. It was a literal inside `routes/Chat.tsx`'s `onLeaveGroup`,
 * which `vitest.config.ts` cannot load at all (plain `node`, no jsdom, no
 * Solid JSX transform) — so the one exit from an unwithdrawable action had no
 * assertion on any client.
 */

import { describe, expect, it } from "vitest";

import {
  LEAVE_GROUP_BODY,
  LEAVE_GROUP_CANCEL_LABEL,
  LEAVE_GROUP_CONFIRM_LABEL,
  LEAVE_GROUP_TITLE,
  leaveGroupConfirmation,
} from "./leave-group-confirm";

describe("the leave-group confirmation", () => {
  it("states what the person loses, in plain words", () => {
    const c = leaveGroupConfirmation({ groupLabel: null });
    expect(c.title).toBe("Leave this group?");
    expect(c.body).toBe("The group and all its messages will be removed from all your devices.");
    expect(LEAVE_GROUP_TITLE()).toBe(c.title);
    expect(LEAVE_GROUP_BODY()).toBe(c.body);
    expect(LEAVE_GROUP_CANCEL_LABEL()).toBe("Cancel");
  });

  it("names the group in the action button", () => {
    // §6.1's rule: the action button's accessible name is its visible label,
    // so it should say what it acts on. §6 fixes the verb at `Leave`; the
    // group's roster label is what §6.1 adds to it.
    expect(leaveGroupConfirmation({ groupLabel: "Ops team" }).confirmLabel).toBe(
      "Leave Ops team",
    );
    expect(LEAVE_GROUP_CONFIRM_LABEL()).toBe("Leave");
  });

  it("falls back to the bare verb for an unnamed group", () => {
    // A chat entry's label can be absent. `Leave ` reads as a truncation and
    // `Leave undefined` as a bug; `Leave` is true either way.
    for (const blank of [null, undefined, "", "  "]) {
      expect(leaveGroupConfirmation({ groupLabel: blank }).confirmLabel).toBe(
        "Leave",
      );
    }
  });

  it("is destructive", () => {
    // §6: `Leave` (destructive) / `Cancel`. The tone is what the dialog reads
    // to colour the action, and it is a decided property rather than a
    // styling choice made in markup.
    expect(leaveGroupConfirmation({ groupLabel: "Ops team" }).tone).toBe(
      "danger",
    );
  });

  it("uses no protocol vocabulary", () => {
    // §1: user-facing copy says `request` and `change`; never proposal,
    // commit, epoch, sweep, committer or leaf. Same rule
    // `remove-member-confirm.test.ts` pins for the sibling flow.
    const c = leaveGroupConfirmation({ groupLabel: "Ops team" });
    const copy = `${c.title} ${c.body ?? ""} ${c.confirmLabel} ${c.cancelLabel}`;
    for (const word of [
      "leaf",
      "proposal",
      "commit",
      "epoch",
      "sweep",
      "committer",
    ]) {
      expect(copy.toLowerCase()).not.toContain(word);
    }
  });
});
