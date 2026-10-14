// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A guest chooses a username and a password (ADR-0075 pass A, part 2).
 *
 * A guest account (the call-link join) has no username and a random
 * password nobody knows. One screen gives it both. The name is claimed in
 * the sign-up's order (the login handle at auth, then the name at
 * discovery); then the password is set with the device's seed-based change,
 * with no proof asked: the account has no known password, this is its
 * first. If the password step fails after the name was saved, the name is
 * kept and only the password is retried.
 *
 * Pure: the entry-point rule, the step machine, and the "first password
 * owed" marker that lets the password step be retried after a reload.
 */
import type { MarkerStorage } from "./phrase-gate";

/** Whether "Choose a username" is offered: only for an account that has
 *  none. */
export function usernameClaimOffered(username: string | null | undefined): boolean {
  return (username ?? "").trim().length === 0;
}

/** Why a step did not go through. */
export type ClaimFailure =
  /** The name is taken (auth `409`, or discovery `409`). */
  | { readonly kind: "taken" }
  /** The login-handle bind limit. */
  | { readonly kind: "bind-limited"; readonly retryAfterSecs: number | null }
  | { readonly kind: "offline" }
  | { readonly kind: "error" };

export type ClaimState =
  /** The form: username and the two password fields. */
  | { readonly step: "form"; readonly working: boolean; readonly failure: ClaimFailure | null }
  /** The name is saved; the password is still owed. The username field is
   *  gone: only the password is retried. */
  | {
      readonly step: "password";
      readonly username: string;
      readonly working: boolean;
      readonly failure: ClaimFailure | null;
    }
  /** Both are saved. */
  | { readonly step: "done"; readonly username: string };

export type ClaimEvent =
  | { readonly type: "submitted" }
  /** The name was claimed and saved on this device. */
  | { readonly type: "name-saved"; readonly username: string }
  | { readonly type: "password-saved" }
  | { readonly type: "failed"; readonly failure: ClaimFailure };

export const CLAIM_START: ClaimState = { step: "form", working: false, failure: null };

/** Where the screen starts: the form for a guest; the password step for an
 *  account whose name is saved and whose first password is still owed (a
 *  reload in between). */
export function claimStart(username: string, firstPasswordOwed: boolean): ClaimState {
  const name = username.trim();
  if (name.length > 0 && firstPasswordOwed) {
    return { step: "password", username: name, working: false, failure: null };
  }
  return CLAIM_START;
}

export function claimReduce(state: ClaimState, event: ClaimEvent): ClaimState {
  if (state.step === "done") return state;
  switch (event.type) {
    case "submitted":
      return { ...state, working: true, failure: null };
    case "name-saved":
      // Still working: the password step follows at once.
      return state.step === "form"
        ? { step: "password", username: event.username, working: true, failure: null }
        : state;
    case "password-saved":
      return state.step === "password" ? { step: "done", username: state.username } : state;
    case "failed":
      // A failure on the password step keeps the name: the step stays.
      return { ...state, working: false, failure: event.failure };
  }
}

/** Which calls a submit makes: the whole claim from the form, only the
 *  password once the name is saved. */
export function claimSubmits(state: ClaimState): "name-and-password" | "password-only" | "nothing" {
  if (state.step === "form") return "name-and-password";
  return state.step === "password" ? "password-only" : "nothing";
}

/** Read a failed call. `status` is `null` when no answer arrived. */
export function claimFailure(input: {
  readonly taken: boolean;
  readonly bindLimited: boolean;
  readonly retryAfterSecs: number | null;
  readonly offline: boolean;
}): ClaimFailure {
  if (input.taken) return { kind: "taken" };
  if (input.bindLimited) return { kind: "bind-limited", retryAfterSecs: input.retryAfterSecs };
  if (input.offline) return { kind: "offline" };
  return { kind: "error" };
}

// ---------------------------------------------------------------------------
// The first password is owed
// ---------------------------------------------------------------------------

function owedKey(accountIdB64: string): string {
  return `lakko:first-password-owed:${accountIdB64}`;
}

/** A guest's name was just saved and its first password is not set yet.
 *  While this holds, the password change runs with no proof (there is no
 *  password to prove). Set BEFORE the name is claimed is not needed: it is
 *  written the moment the name is saved. */
export function markFirstPasswordOwed(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.setItem(owedKey(accountIdB64), "1");
  } catch {
    // Storage blocked: the in-memory fact still holds for this page.
  }
}

export function isFirstPasswordOwed(storage: MarkerStorage | undefined, accountIdB64: string): boolean {
  try {
    return storage?.getItem(owedKey(accountIdB64)) === "1";
  } catch {
    return false;
  }
}

export function clearFirstPasswordOwed(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(owedKey(accountIdB64));
  } catch {
    // Storage blocked: nothing was recorded either.
  }
}
