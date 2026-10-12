// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a route guard renders, decided in one place: the steps that come
 * before any route. New recovery words (after the account keys were changed
 * on this device) are shown and acknowledged first; then the sign-up's
 * "Protect your account" step (ADR-0067, `docs/ux/identity-recovery.md`
 * 19.1a); then the new-password step.
 *
 * ## Why the guards own these steps
 *
 * `auth.signup` flips the auth state to `session` before it returns, and
 * `/signup` sits inside `<RequireAnonymous>`. The guard sees `session` and
 * navigates home in the same tick, unmounting the signup page, so a step
 * that follows sign-up cannot live on that page. The auth context holds the
 * step as owed until the person finished it, and BOTH guards render it while
 * it is owed, so it is reached from `/signup`, from a protected route the
 * user navigates to with the back button, and after a reload.
 *
 * ADR-0067 D2: the recovery phrase is NOT shown at sign-up. The phrase
 * screen (`save-signup-phrase`, named before that decision) is for new words
 * only: a key reset, taking the account back, a key rotation, "Save a
 * recovery phrase".
 *
 * The two ADR-0017 identity states that already worked the same way
 * (`needs-recovery-phrase`, `save-fresh-phrase` — which also covers the
 * ADR-0032 guest signup, whose phrase is parked before the session lands) are
 * folded into the same decision so the three cannot drift apart.
 *
 * Pure: vitest runs in plain node without the Solid transform, so this is the
 * only form in which the rule can be tested.
 */

/** The auth state discriminator, as the guards read it. */
export type GuardAuthKind =
  | "loading"
  | "anon"
  | "session"
  | "needs-recovery-phrase"
  | "save-fresh-phrase";

/** What a guard renders. */
export type GuardView =
  /** The auth context is still resolving. */
  | "loading"
  /** The guarded route itself. */
  | "children"
  /** ADR-0017 — authenticated, identity not yet on this device. */
  | "restore-identity"
  /** ADR-0017 lost-everything / ADR-0032 guest — a new phrase to save. */
  | "save-fresh-phrase"
  /** New recovery words (the account keys were changed on this device),
   *  not yet acknowledged. */
  | "save-signup-phrase"
  /** ADR-0067 (UX 19.1a): the sign-up's "Protect your account" step. */
  | "protect-account"
  /** docs/ux/identity-recovery.md 7.1 — a forgot-password route succeeded
   *  on a device that holds the keys: choose a new password, or "Not now". */
  | "choose-new-password"
  /** Navigate to the login page. */
  | "redirect-login"
  /** Navigate home. */
  | "redirect-home";

/**
 * Views both guards share, or `null` when the guard's own rule applies.
 *
 * `newPasswordOffered` is the step after a forgot-password route (an
 * authenticator code, a passkey, another device) on a device that
 * holds the keys. It is shown in a live session only, and after owed new
 * words, which can never be skipped.
 *
 * `protectStepOwed` is the sign-up's "Protect your account" step: after new
 * words (choosing "Save a recovery phrase" on that step ends in them),
 * before everything else.
 */
function sharedView(
  kind: GuardAuthKind,
  signupPhrasePending: boolean,
  newPasswordOffered: boolean,
  protectStepOwed: boolean,
): GuardView | null {
  switch (kind) {
    case "loading":
      return "loading";
    case "needs-recovery-phrase":
      return "restore-identity";
    case "save-fresh-phrase":
      return "save-fresh-phrase";
    case "session":
      // Only a live session can have an unacknowledged signup phrase; a
      // pending flag left over in any other state is not a phrase to show.
      if (signupPhrasePending) return "save-signup-phrase";
      if (protectStepOwed) return "protect-account";
      return newPasswordOffered ? "choose-new-password" : null;
    case "anon":
      return null;
  }
}

/** The guard around protected routes (`<RequireSession>`). */
export function sessionGuardView(
  kind: GuardAuthKind,
  signupPhrasePending: boolean,
  newPasswordOffered = false,
  protectStepOwed = false,
): GuardView {
  return (
    sharedView(kind, signupPhrasePending, newPasswordOffered, protectStepOwed) ??
    (kind === "session" ? "children" : "redirect-login")
  );
}

/** The guard around the public auth pages (`<RequireAnonymous>`). */
export function anonymousGuardView(
  kind: GuardAuthKind,
  signupPhrasePending: boolean,
  newPasswordOffered = false,
  protectStepOwed = false,
): GuardView {
  return (
    sharedView(kind, signupPhrasePending, newPasswordOffered, protectStepOwed) ??
    (kind === "anon" ? "children" : "redirect-home")
  );
}

// ---------------------------------------------------------------------------
// Surviving a reload
// ---------------------------------------------------------------------------

/** The subset of `Storage` the marker needs. */
export interface MarkerStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function markerKey(accountIdB64: string): string {
  return `lakko:signup-phrase-unacknowledged:${accountIdB64}`;
}

/**
 * Record that `accountIdB64` has new recovery words the user has not yet
 * acknowledged. The phrase itself is NOT written here — it is already cached
 * under the device root key by the backup bootstrap (`backup/phrase-store.ts`),
 * and a reload reads it back from there. Only the fact that it is owed is kept
 * in plain storage.
 */
export function markSignupPhraseOwed(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.setItem(markerKey(accountIdB64), "1");
  } catch {
    // Storage blocked: the in-memory gate still holds for this page lifetime.
  }
}

/** Whether a reload of `accountIdB64` still owes the phrase screen. */
export function isSignupPhraseOwed(storage: MarkerStorage | undefined, accountIdB64: string): boolean {
  try {
    return storage?.getItem(markerKey(accountIdB64)) === "1";
  } catch {
    return false;
  }
}

/** Clear the marker — on acknowledgement, sign-out and device wipe. */
export function clearSignupPhraseOwed(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(markerKey(accountIdB64));
  } catch {
    // Storage blocked: nothing was recorded either.
  }
}
