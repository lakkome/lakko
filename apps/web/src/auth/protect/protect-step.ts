// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The sign-up's "Protect your account" step (ADR-0067 D2 and D7,
 * `docs/ux/identity-recovery.md` 19.1a): which rows it offers and what "Not
 * now" asks. Pure.
 */
import type { ConfirmRequest } from "../../ui/confirm-dialog";
import { t } from "../../l10n";
import type { RecoveryEmailStatus } from "./account-protection";

export type ProtectChoice = "email" | "passkey" | "phrase";

/**
 * The rows, in order: email, passkey, phrase.
 *
 *   - `Add an email` is absent when mail is not set up on this server (UX
 *     19.2) or while that is not known yet, and for an account without a
 *     username (the stored value is made from the username).
 *   - `Create a passkey` is shown only where this browser can use passkeys,
 *     and for an account with a username (a passkey is registered under it).
 */
export function protectStepRows(
  status: RecoveryEmailStatus | null,
  hasUsername: boolean,
  passkeysSupported: boolean,
): ProtectChoice[] {
  const rows: ProtectChoice[] = [];
  if (status !== null && status.available && hasUsername) rows.push("email");
  if (passkeysSupported && hasUsername) rows.push("passkey");
  rows.push("phrase");
  return rows;
}

/** Which rows are done, and so marked `protect.done`. */
export function protectStepDone(facts: {
  readonly status: RecoveryEmailStatus | null;
  /** A passkey was created on this step, or the account already has one. */
  readonly hasPasskey: boolean;
  readonly phraseProtectedHere: boolean;
}): ProtectChoice[] {
  const done: ProtectChoice[] = [];
  const st = facts.status;
  if (st !== null && st.enabled && (st.vault || st.phraseSaved)) done.push("email");
  if (facts.hasPasskey) done.push("passkey");
  if (facts.phraseProtectedHere || st?.phraseSaved === true) done.push("phrase");
  return done;
}

/** What stands under the rows: the plain `Not now` (which asks first), or,
 *  once any row is done, the primary `Continue` (which asks nothing). */
export function protectStepExit(done: readonly ProtectChoice[]): "not-now" | "continue" {
  return done.length > 0 ? "continue" : "not-now";
}

/**
 * Whether the step is over without the person pressing anything: a phrase
 * was saved (here or on another device). Saving a phrase lands in the app
 * directly: its new words follow, and never this step again. An email or a
 * passkey does not end the step; the row is marked done and `Continue` does.
 */
export function protectStepSettled(status: RecoveryEmailStatus | null, phraseProtectedHere: boolean): boolean {
  return phraseProtectedHere || status?.phraseSaved === true;
}

/**
 * "Not now": the app's own confirmation. Its primary action is the SAFE one
 * (`Add an email`), so `requestConfirm` resolving `true` means "add an
 * email" and `false` means "continue without".
 *
 * `emailOffered` is whether the step has the email row: without it the
 * primary action is to stay on the step.
 */
export function skipProtectConfirmation(emailOffered: boolean): ConfirmRequest {
  return {
    title: t("protect.skip.confirm.title"),
    body: t("protect.skip.confirm.body"),
    confirmLabel: emailOffered ? t("protect.email.title") : t("common.back"),
    cancelLabel: t("protect.skip.confirm.continue"),
    tone: "neutral",
  };
}
