// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0037 F5a — the stale-backup-head restore override.
 *
 * The odd one out among the confirmations, and the only one that is part of a
 * SECURITY gate rather than a UX courtesy: it is the user-acknowledged arm of
 * the ADR-0037 restore verdict, which detects that a storage tier has handed
 * back an older backup than the account's current head. Refusing is the safe
 * answer, so this is the one dialog whose `false` is a real defence and not
 * merely an unstarted action.
 *
 * That is why the copy is pinned harder than the others: the user is being
 * asked to override a rollback detection, and the sentence describing what a
 * rollback costs them is the whole basis for the decision.
 */

import { describe, expect, it } from "vitest";

import {
  STALE_RESTORE_BODY,
  STALE_RESTORE_CANCEL_LABEL,
  STALE_RESTORE_CONFIRM_LABEL,
  STALE_RESTORE_TITLE,
  staleRestoreConfirmation,
} from "./stale-restore-confirm";

describe("the stale-backup restore confirmation", () => {
  it("carries the reason the head was judged stale", () => {
    // The verdict's own reason string is what distinguishes "older than the
    // staleness window" from "below the version floor", and the user cannot
    // weigh the override without it.
    const c = staleRestoreConfirmation({ reason: "signed 45 days ago" });
    expect(c.body).toContain("signed 45 days ago");
    expect(c.body).toContain(STALE_RESTORE_BODY);
  });

  it("states what a rollback costs, in the terms ADR-0037 names", () => {
    // Older messages, and devices the user removed reappearing — the second
    // is the one with a security consequence, so it is named explicitly
    // rather than folded into "older state".
    expect(STALE_RESTORE_BODY).toContain("older messages");
    expect(STALE_RESTORE_BODY).toContain("devices you removed may reappear");
  });

  it("labels the action as the override it is", () => {
    // Not `Restore` — the point is that the user is knowingly proceeding past
    // a refusal, and the label says so. `Restore anyway` names the thing
    // (the restore) and marks it as the override in the same breath.
    const c = staleRestoreConfirmation({ reason: "r" });
    expect(c.confirmLabel).toBe("Restore anyway");
    expect(STALE_RESTORE_CONFIRM_LABEL).toBe("Restore anyway");
    expect(c.cancelLabel).toBe("Cancel");
    expect(STALE_RESTORE_CANCEL_LABEL).toBe("Cancel");
  });

  it("asks a question rather than announcing a fact", () => {
    const c = staleRestoreConfirmation({ reason: "r" });
    expect(c.title).toBe(STALE_RESTORE_TITLE);
    expect(c.title).toBe("Restore this older backup?");
  });

  it("is destructive", () => {
    // Overriding the gate rolls the account back. It is not a neutral choice
    // and must not be coloured as one.
    expect(staleRestoreConfirmation({ reason: "r" }).tone).toBe("danger");
  });
});
