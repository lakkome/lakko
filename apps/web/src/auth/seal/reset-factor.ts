// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The reset's second factor (ADR-0073, amended 2026-11-26;
 * `docs/ux/identity-recovery.md` 15.1b): the rules of the section, pure.
 *
 * A key reset needs the password, proven by a PASSWORD sign-in in the last
 * ten minutes (a passkey sign-in no longer stands in for it), and one of the
 * factors the account has: a code from its authenticator app, a code mailed
 * to its recovery email, or one of its passkeys. A confirmed factor is a
 * grant the server mints, valid for ten minutes, held in memory only and
 * never logged; the reset spends it.
 */
import type { AccountFactors } from "../protect/factors";

export type ResetFactorKind = "totp" | "email" | "passkey";

/** The machine code of the server's `403` when a reset (or the request for
 *  the owned-channels server half) carries no usable reset factor grant. */
export const CODE_RESET_FACTOR_REQUIRED = "reset-factor-required";
/** The one `401` of every factor route: a wrong code, a decoy or spent
 *  challenge, no such factor on the account. */
export const CODE_FACTOR_REFUSED = "factor-refused";

/** A mailed code is tried at most this many times (the server's count). */
export const EMAIL_CODE_TRIES = 5;

// ---------------------------------------------------------------------------
// The holder
// ---------------------------------------------------------------------------

export interface HeldResetGrant {
  readonly grant: string;
  readonly factor: ResetFactorKind;
  readonly expiresAtMs: number;
}

/** The in-memory holder of at most one reset factor grant. */
export interface ResetGrantHolder {
  put(accountIdB64: string, held: HeldResetGrant): void;
  /** The grant for `accountIdB64` when one is held and not expired at
   *  `nowMs`. An expired one, and another account's, is dropped by the read. */
  live(accountIdB64: string, nowMs: number): HeldResetGrant | null;
  /** Drop the grant if it is the one given (the server refused it). */
  spend(grant: string): void;
  clear(): void;
}

export function createResetGrantHolder(): ResetGrantHolder {
  let held: { accountIdB64: string; grant: HeldResetGrant } | null = null;
  return {
    put(accountIdB64, grant) {
      held = { accountIdB64, grant };
    },
    live(accountIdB64, nowMs) {
      if (held === null) return null;
      if (held.accountIdB64 !== accountIdB64 || nowMs >= held.grant.expiresAtMs) {
        held = null;
        return null;
      }
      return held.grant;
    },
    spend(grant) {
      if (held !== null && held.grant.grant === grant) held = null;
    },
    clear() {
      held = null;
    },
  };
}

// ---------------------------------------------------------------------------
// Errors the auth layer throws to the section
// ---------------------------------------------------------------------------

/** No live reset factor grant: the section opens again, with no error
 *  styling (nothing went wrong). */
export class ResetFactorRequiredError extends Error {
  override name = "ResetFactorRequiredError";
  constructor() {
    super("a second confirmation is needed to reset the account keys");
  }
}

/** The factor was refused: a wrong code, or a factor the account lacks. */
export class ResetFactorRefusedError extends Error {
  override name = "ResetFactorRefusedError";
  constructor() {
    super("the confirmation was not accepted");
  }
}

// ---------------------------------------------------------------------------
// The section
// ---------------------------------------------------------------------------

/**
 * The rows, in the fixed order authenticator, email, passkey: the factors
 * this account has. A row the account does not have is absent, never
 * disabled; `Passkey` is also absent where this browser cannot use passkeys.
 */
export function resetFactorRows(factors: AccountFactors, passkeysSupported: boolean): ResetFactorKind[] {
  const rows: ResetFactorKind[] = [];
  if (factors.totp) rows.push("totp");
  if (factors.email) rows.push("email");
  if (factors.passkeys > 0 && passkeysSupported) rows.push("passkey");
  return rows;
}

export type ResetSection =
  /** The factors are not known yet: nothing of the section is drawn, and no
   *  panel claims the account cannot reset. */
  | { readonly kind: "loading" }
  /** The account has none of the three: the "cannot reset" panel with the
   *  `body_none` text and no `Back`. */
  | { readonly kind: "no-factor"; readonly accountHasNone: true }
  /** `I cannot use any of these`: the same panel with `Back`. */
  | { readonly kind: "no-factor"; readonly accountHasNone: false }
  /** One line with a check mark; the word and the button are usable. */
  | { readonly kind: "confirmed"; readonly factor: ResetFactorKind }
  /** The password block (unless a password proof is live) and the rows. */
  | {
      readonly kind: "choose";
      readonly showPassword: boolean;
      readonly rows: readonly ResetFactorKind[];
    };

export function resetSection(input: {
  readonly factors: AccountFactors | null;
  readonly passkeysSupported: boolean;
  /** A password sign-in less than ten minutes ago. */
  readonly passwordProofLive: boolean;
  /** The factor of the live reset factor grant, or `null`. */
  readonly confirmed: ResetFactorKind | null;
  /** `I cannot use any of these` was chosen. */
  readonly cannotUse: boolean;
}): ResetSection {
  if (input.confirmed !== null) return { kind: "confirmed", factor: input.confirmed };
  if (input.factors === null) return { kind: "loading" };
  const rows = resetFactorRows(input.factors, input.passkeysSupported);
  const accountHasNone = !input.factors.totp && !input.factors.email && input.factors.passkeys === 0;
  if (accountHasNone) return { kind: "no-factor", accountHasNone: true };
  if (input.cannotUse || rows.length === 0) return { kind: "no-factor", accountHasNone: false };
  return { kind: "choose", showPassword: !input.passwordProofLive, rows };
}

/** `Reset account keys` is enabled only when a factor is confirmed and the
 *  word is typed. */
export function resetSubmitEnabled(input: {
  readonly section: ResetSection;
  readonly wordTyped: boolean;
  readonly busy: boolean;
}): boolean {
  return input.section.kind === "confirmed" && input.wordTyped && !input.busy;
}

/** The catalog key of the confirmed line. */
export function resetFactorDoneKey(factor: ResetFactorKind): string {
  return `proof.reset.factor.done.${factor}`;
}

/** The catalog key of a row's title. */
export function resetFactorRowKey(factor: ResetFactorKind): string {
  return `proof.reset.factor.${factor}`;
}

// ---------------------------------------------------------------------------
// Failures
// ---------------------------------------------------------------------------

export type ResetFactorFailure =
  /** `401 factor-refused`. */
  | { readonly kind: "refused" }
  /** `403 fresh-auth-required`: the password proof ran out; the password
   *  block is drawn again, with no error styling. */
  | { readonly kind: "password-needed" }
  | { readonly kind: "rate-limited"; readonly retryAfterSecs: number }
  /** The letter could not be sent, or mail is not set up (`503`). */
  | { readonly kind: "send-failed" }
  | { readonly kind: "invalid-address" }
  | { readonly kind: "offline" }
  | { readonly kind: "generic" };

export function resetFactorFailure(answer: {
  readonly status: number | null;
  readonly errorCode: string | null;
  readonly retryAfterSecs: number | null;
  readonly network: boolean;
}): ResetFactorFailure {
  if (answer.network || answer.status === null) return { kind: "offline" };
  if (answer.status === 401 && answer.errorCode === CODE_FACTOR_REFUSED) return { kind: "refused" };
  if (answer.status === 403 && answer.errorCode === "fresh-auth-required") return { kind: "password-needed" };
  if (answer.status === 429) return { kind: "rate-limited", retryAfterSecs: Math.max(1, answer.retryAfterSecs ?? 60) };
  if (answer.status === 400 && answer.errorCode === "address-invalid") return { kind: "invalid-address" };
  if (answer.status === 503) return { kind: "send-failed" };
  return { kind: "generic" };
}

/** What the email row shows after a refused code: the plain "not right"
 *  line, or, once the tries of this code are used up, "send a new code"
 *  (the row returns to its `Send code` state). */
export function emailCodeRefusal(triesUsed: number): "wrong" | "used-up" {
  return triesUsed >= EMAIL_CODE_TRIES ? "used-up" : "wrong";
}

/** Whether a mailed code is past its lifetime at `nowMs`. */
export function emailCodeExpired(sentAtMs: number, expiresInSecs: number, nowMs: number): boolean {
  return expiresInSecs > 0 && nowMs - sentAtMs >= expiresInSecs * 1000;
}
