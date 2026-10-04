// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The password-fresh reset grant (ADR-0065, amended 2026-10-04;
 * docs/ux/identity-recovery.md section 15.1a).
 *
 * "Reset account keys" needs the account password proven just now: the
 * server refuses both `POST /account/keys/reset/*` calls without a reset
 * grant, and only the PASSWORD sign-in (`POST /login/finish`) mints one
 * (ten minutes by default, one reset). A stolen session, or an unlocked
 * device in someone else's hands, cannot reset the keys.
 *
 * The grant is an opaque string to the client. It is held IN MEMORY ONLY:
 * never written to any store and never logged. It is bound to the account
 * it was issued for, dropped on sign-out and on the account wipe, dropped
 * when a reset used it, and not handed out once expired.
 *
 * This module is pure (the caller passes the clock), so the holder, the
 * screen's decision and the error mapping are pinned by tests.
 */

/** A grant as the password login's finish returned it. */
export interface ResetGrant {
  /** Opaque; sent back verbatim as `reset_grant`. Never parsed. */
  readonly grant: string;
  /** Absolute expiry, unix milliseconds. */
  readonly expiresAtMs: number;
}

/** The two fields `POST /login/finish` adds (password login only). */
export interface ResetGrantWire {
  reset_grant?: unknown;
  reset_grant_expires_at_ms?: unknown;
}

/** The machine code of the server's `403` when a reset carries no usable
 *  grant (missing, malformed, expired, another account's, stale generation). */
export const CODE_PASSWORD_REQUIRED = "password-required";

/**
 * Read the grant out of a password login's finish response, or `null` when
 * the response carries none or carries one of the wrong shape. Never
 * throws: a sign-in must not fail because the grant is absent (an older
 * server, or a route that mints none); the reset screen then asks for the
 * password.
 */
export function resetGrantOf(wire: ResetGrantWire | null | undefined): ResetGrant | null {
  if (wire === null || wire === undefined) return null;
  const grant = wire.reset_grant;
  const expiresAtMs = wire.reset_grant_expires_at_ms;
  if (typeof grant !== "string" || grant.length === 0) return null;
  if (typeof expiresAtMs !== "number" || !Number.isFinite(expiresAtMs) || expiresAtMs <= 0) {
    return null;
  }
  return { grant, expiresAtMs };
}

/** The in-memory holder of at most one grant. */
export interface ResetGrantHolder {
  /** Keep `grant` for `accountIdB64`, replacing whatever was held. */
  put(accountIdB64: string, grant: ResetGrant): void;
  /** The grant string for `accountIdB64` when one is held and not expired
   *  at `nowMs`; otherwise `null`. An expired grant, and a grant of another
   *  account, are dropped by the read. */
  live(accountIdB64: string, nowMs: number): string | null;
  /** Drop the grant if it is the one given (a reset used it, or the server
   *  refused it). A newer grant put meanwhile is kept. */
  spend(grant: string): void;
  /** Drop whatever is held (sign-out, account wipe). */
  clear(): void;
}

export function createResetGrantHolder(): ResetGrantHolder {
  let held: { readonly accountIdB64: string; readonly grant: ResetGrant } | null = null;
  return {
    put(accountIdB64, grant) {
      held = { accountIdB64, grant };
    },
    live(accountIdB64, nowMs) {
      if (held === null) return null;
      if (held.accountIdB64 !== accountIdB64 || held.grant.expiresAtMs <= nowMs) {
        held = null;
        return null;
      }
      return held.grant.grant;
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
// The warning screen's decision (UX 15.1 + 15.1a)
// ---------------------------------------------------------------------------

export interface ResetFormInput {
  /** A live grant is held for this account right now. */
  readonly grantLive: boolean;
  /** The screen already revealed the password field (it stays once shown:
   *  a field must not vanish under the person's hands). */
  readonly passwordRevealed: boolean;
  /** The account's username is known on this device. When it is not (a
   *  passkey sign-in that left no name), the password sign-in needs it. */
  readonly usernameKnown: boolean;
  readonly password: string;
  readonly username: string;
  /** The confirmation word was typed (`resetConfirmed`). */
  readonly confirmed: boolean;
  /** A reset is running, or the screen is otherwise busy or waiting out a
   *  rate limit. */
  readonly blocked: boolean;
}

export interface ResetFormState {
  /** Show the `Password` field (with its helper) above the confirmation word. */
  readonly showPassword: boolean;
  /** Show a username field above the password (only with the password field). */
  readonly showUsername: boolean;
  /** The reset button is enabled. */
  readonly canSubmit: boolean;
}

/**
 * What the warning screen shows and whether its button is enabled. With a
 * live grant the screen is exactly section 15.1 (no field, the word alone
 * enables the button). Without one the password (and the username when the
 * device does not know it) must be filled as well.
 */
export function resetFormState(input: ResetFormInput): ResetFormState {
  const showPassword = input.passwordRevealed || !input.grantLive;
  const showUsername = showPassword && !input.usernameKnown;
  const filled =
    input.confirmed &&
    (!showPassword || input.password.length > 0) &&
    (!showUsername || input.username.trim().length > 0);
  return { showPassword, showUsername, canSubmit: filled && !input.blocked };
}

// ---------------------------------------------------------------------------
// Failure mapping
// ---------------------------------------------------------------------------

/** The reset needs the password and none was given: no grant is held (or
 *  the server refused the one sent with `403 password-required`). Nothing
 *  was reset. The screen reveals the password field; it is not an error. */
export class ResetPasswordRequiredError extends Error {
  override name = "ResetPasswordRequiredError";
  constructor() {
    super("resetting the account keys needs the password");
  }
}

/** The password typed on the reset screen did not sign in. Nothing was reset. */
export class ResetWrongPasswordError extends Error {
  override name = "ResetWrongPasswordError";
  constructor() {
    super("the password did not match");
  }
}

/** `true` for the server's `403 {"error":"password-required"}`. */
export function isPasswordRequired(input: {
  readonly status: number | null;
  readonly errorCode: string | null;
}): boolean {
  return input.status === 403 && input.errorCode === CODE_PASSWORD_REQUIRED;
}

/** How the password sign-in in front of a reset ended, when it failed. */
export type ResetLoginFailure =
  /** The password is wrong (or the name is nobody's: the same answer). */
  | "wrong-password"
  /** Not an answer about the password: a limit, an outage, the network.
   *  The caller rethrows the original error for the screen's usual lines. */
  | "other";

/**
 * Classify a failed password sign-in. A wrong password fails the exchange
 * on this side (the core refuses to finish it: an error that is neither an
 * HTTP status nor a network failure) or at the server (`401`; `404` from a
 * login state that is gone is read the same way, as `verifyPassword` does).
 */
export function resetLoginFailure(input: {
  /** The HTTP status when the server answered, else `null`. */
  readonly status: number | null;
  /** The request never got an answer. */
  readonly network: boolean;
}): ResetLoginFailure {
  if (input.network) return "other";
  if (input.status === null) return "wrong-password";
  return input.status === 401 || input.status === 404 ? "wrong-password" : "other";
}
