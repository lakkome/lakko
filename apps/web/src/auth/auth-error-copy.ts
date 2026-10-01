// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the auth screens say when something fails (docs/ux/client-redesign.md
 * section 6): one sentence on what happened, one on what to do, never the
 * transport or the protocol.
 *
 * `AuthContext` keeps throwing what it throws; this maps those errors to
 * screen copy at the edge, so the enumeration-safe collapse it performs is
 * preserved: an unknown username and a wrong password reach the user as the
 * SAME sentence, and so do the corresponding server statuses.
 *
 * Pure, so the collapse is pinned by a test.
 */
import { AuthHttpError, AuthNetworkError } from "./client";
import { DiscoveryHttpError, DiscoveryNetworkError } from "../discovery/client";
import { isAnonTokenNetworkFailure } from "../tokens/anon-token-pool";

const OFFLINE = "Could not reach Lakko. Check your connection and try again.";
const UNAVAILABLE = "That is not available right now. Try again later.";

// ---------------------------------------------------------------------------
// Server limits (ADR-0054 S4, docs/ux/anti-abuse.md section 2)
//
// Every auth limit answers `429 {"error": code, "retry_after_secs": N}` with
// `Retry-After: N`. A limit is the server's decision: it never signs anyone
// out, never reads as "invalid", and says when to try again.
// ---------------------------------------------------------------------------

/** `too-many-attempts`: login, recovery login, reset, TOTP, three words,
 *  device authorize. */
export const TOO_MANY_ATTEMPTS = "Too many attempts. Wait a moment, then try again.";
/** `recovery-code-send-limited`: the email / SMS code send. */
export const RECOVERY_CODE_SEND_LIMITED =
  "We sent a code recently. Wait a few minutes before asking for another.";
/** `device-request-limited`: "Is this you?" prompts on the other devices. */
export const DEVICE_REQUEST_LIMITED =
  "Too many requests to your other devices. Try again in a few minutes.";
/** `signup-rate-limited`: signup start or its challenge. */
export const SIGNUP_RATE_LIMITED =
  "Too many new accounts from this network right now. Try again later.";

/** `handle-bind-limited` (`PUT /auth/login-handle`, added 2026-09-28):
 *  choosing or changing a username too often. In minutes, rounded up; over
 *  an hour (or with no wait given) it says "later" instead of a number. */
export function handleBindLimitedMessage(retryAfterSecs: number | null): string {
  const lead = "You have changed your username too often.";
  if (retryAfterSecs === null || retryAfterSecs > 3600) return `${lead} Try again later.`;
  const minutes = Math.max(1, Math.ceil(retryAfterSecs / 60));
  return `${lead} Try again in ${minutes} ${minutes === 1 ? "minute" : "minutes"}.`;
}

/** `true` when `err` is auth's `429 handle-bind-limited`. */
export function isHandleBindLimited(err: unknown): boolean {
  return err instanceof AuthHttpError && err.status === 429 && err.errorCode === "handle-bind-limited";
}

/** The attempts copy, in minutes when the server asks for more than one. */
export function tooManyAttemptsMessage(retryAfterSecs: number | null): string {
  if (retryAfterSecs !== null && retryAfterSecs > 60) {
    return `Too many attempts. Try again in ${Math.ceil(retryAfterSecs / 60)} minutes.`;
  }
  return TOO_MANY_ATTEMPTS;
}

/**
 * The copy for a server limit, or `null` when `err` is not one. Covers the
 * 429 codes and the `503 rate-limiter-unavailable` (the limiter's store is
 * down: fail closed, shown as the generic failure).
 */
export function authLimitMessage(err: unknown): string | null {
  if (!(err instanceof AuthHttpError)) return null;
  const code = err.errorCode;
  if (err.status === 503 && code === "rate-limiter-unavailable") return UNAVAILABLE;
  if (err.status !== 429) return null;
  switch (code) {
    case "recovery-code-send-limited":
      return RECOVERY_CODE_SEND_LIMITED;
    case "device-request-limited":
      return DEVICE_REQUEST_LIMITED;
    case "signup-rate-limited":
      return SIGNUP_RATE_LIMITED;
    case "handle-bind-limited":
      return handleBindLimitedMessage(err.retryAfter);
    default:
      return tooManyAttemptsMessage(err.retryAfter);
  }
}

/** `true` when `err` is the signup's own network limit, so the form offers
 *  `Try again` and keeps its contents. */
export function isSignupRateLimited(err: unknown): boolean {
  return err instanceof AuthHttpError && err.status === 429 && err.errorCode === "signup-rate-limited";
}

/** The one sentence a failed password login shows, whatever failed. */
export const WRONG_CREDENTIALS = "Wrong username or password. Check them and try again.";

function isOffline(err: unknown): boolean {
  return err instanceof AuthNetworkError || err instanceof DiscoveryNetworkError;
}

function status(err: unknown): number | null {
  if (err instanceof AuthHttpError || err instanceof DiscoveryHttpError) return err.status;
  return null;
}

/** A message the context already wrote for the user (sentence case, ends
 *  with a full stop) is passed through; a bare diagnostic is not. */
function isScreenCopy(message: string): boolean {
  return /^[A-Z]/.test(message) && /[.!?]$/.test(message) && !message.includes("→");
}

/** The login form. */
export function loginErrorMessage(err: unknown): string {
  if (isOffline(err)) return OFFLINE;
  const limit = authLimitMessage(err);
  if (limit !== null) return limit;
  // The username lookup could not get an anonymous token (ADR-0054 S3).
  if (isAnonTokenNetworkFailure(err)) return OFFLINE;
  const s = status(err);
  if (s === 503) return UNAVAILABLE;
  // 401 / 404 / a lookup miss / "invalid credentials": all the same sentence.
  if (s === 401 || s === 404 || s === 400) return WRONG_CREDENTIALS;
  if (err instanceof Error) {
    if (err.message === "invalid credentials") return WRONG_CREDENTIALS;
    if (isScreenCopy(err.message)) return err.message;
  }
  return WRONG_CREDENTIALS;
}

/** The passkey button on the login form. The context already writes these
 *  as sentences; the fallback covers a bare diagnostic. */
export function passkeyErrorMessage(err: unknown): string {
  if (isOffline(err)) return OFFLINE;
  if (err instanceof Error && isScreenCopy(err.message)) return err.message;
  return "Could not sign in with the passkey. Try again, or use your password.";
}

/** The signup form. */
export function signupErrorMessage(err: unknown): string {
  if (isOffline(err)) return OFFLINE;
  const limit = authLimitMessage(err);
  if (limit !== null) return limit;
  if (isAnonTokenNetworkFailure(err)) return OFFLINE;
  const s = status(err);
  if (s === 503) return UNAVAILABLE;
  if (s === 409 || (err instanceof Error && /already taken/.test(err.message))) {
    return "That username is taken. Choose another one.";
  }
  if (err instanceof Error && isScreenCopy(err.message)) return err.message;
  return "Could not create the account. Try again.";
}

/** The recovery forms that take the phrase (or the shares) and a new
 *  password. A wrong phrase and an unknown username are one sentence. */
export function recoverErrorMessage(err: unknown): string {
  if (isOffline(err)) return OFFLINE;
  const limit = authLimitMessage(err);
  if (limit !== null) return limit;
  if (isAnonTokenNetworkFailure(err)) return OFFLINE;
  const s = status(err);
  if (s === 503) return UNAVAILABLE;
  if (s === 401 || s === 404 || s === 400) {
    return "That did not match this account. Check the words and the username, then try again.";
  }
  if (err instanceof Error) {
    if (/username not found|invalid credentials/.test(err.message)) {
      return "That did not match this account. Check the words and the username, then try again.";
    }
    if (isScreenCopy(err.message)) return err.message;
  }
  return "Could not recover the account. Check the words and try again.";
}
