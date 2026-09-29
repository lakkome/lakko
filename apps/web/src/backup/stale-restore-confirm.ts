// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0037 F5a — the stale-backup-head restore override.
 *
 * ## The odd one out, and why it is worth saying so
 *
 * Every other confirmation in this client guards an action the user is
 * choosing to take. This one guards an action the SYSTEM has already refused:
 * it is the user-acknowledged arm of the ADR-0037 restore verdict, raised
 * when the head gate detects that the storage tier handed back an older
 * backup than the account's current head — a rollback that silently
 * resurrects revoked devices and forgets recently-accepted contacts.
 *
 * So it is the one dialog whose `false` is a real defence rather than merely
 * an unstarted action, and the fail-closed posture its `window.confirm`
 * default carried (*"a headless env with no `window.confirm` returns `false`
 * — never a silent downgrade"*) has to survive the move. It does, and by the
 * same rule rather than a special case: `requestConfirm` resolves `false`
 * whenever no host is mounted.
 *
 * ## Why the widget change nevertheless helps here
 *
 * This is the confirmation a user is LEAST equipped to answer — it asks them
 * to override a security gate on the strength of one sentence — and it was
 * the one rendered with the least care: a browser dialog with a chrome title
 * above the app's own explanation. It is also the site where suppression bit
 * hardest, since a suppressed dialog made a legitimate restore impossible
 * with no way to find out why.
 */

import { type ConfirmRequest } from "../ui/confirm-dialog";
import { t } from "../l10n";

// The copy lives in the catalog (ADR-0058); each accessor renders it in the
// language current when the dialog is requested.

/** The question. */
export const staleRestoreTitle = (): string => t("backup.stale_restore.title");

/**
 * The cost of the override, in ADR-0037's own terms.
 *
 * `devices you removed may reappear` is named explicitly rather than folded
 * into "older state" because it is the half with a security consequence — a
 * rollback that reinstates a revoked device is the reason the head gate
 * exists.
 */
export const staleRestoreConsequence = (): string => t("backup.stale_restore.consequence");

/**
 * The action.
 *
 * Not `Restore`: the user is knowingly proceeding past a refusal, and the
 * label says so. `Restore anyway` names the thing and marks it as the
 * override in one phrase.
 */
export const staleRestoreConfirmLabel = (): string => t("backup.stale_restore.confirm");

/** The exit — and the safe answer. */
export const staleRestoreCancelLabel = (): string => t("common.cancel");

/**
 * The confirmation for overriding a stale-head verdict.
 *
 * The verdict's own `reason` is carried into the body because it is what
 * distinguishes "older than the staleness window" from "below the version
 * floor", and the user cannot weigh the override without it.
 */
export function staleRestoreConfirmation(input: {
  /** The verdict's reason string, as produced by the head gate. */
  readonly reason: string;
}): ConfirmRequest {
  return {
    title: staleRestoreTitle(),
    body: t("backup.stale_restore.body", {
      reason: input.reason,
      consequence: staleRestoreConsequence(),
    }),
    confirmLabel: staleRestoreConfirmLabel(),
    cancelLabel: staleRestoreCancelLabel(),
    tone: "danger",
  };
}
