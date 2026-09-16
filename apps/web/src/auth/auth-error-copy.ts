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

const OFFLINE = "Could not reach Lakko. Check your connection and try again.";
const TOO_MANY = "Too many attempts. Wait a few minutes and try again.";
const UNAVAILABLE = "That is not available right now. Try again later.";

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
  const s = status(err);
  if (s === 429) return TOO_MANY;
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
  const s = status(err);
  if (s === 429) return TOO_MANY;
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
  const s = status(err);
  if (s === 429) return TOO_MANY;
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
