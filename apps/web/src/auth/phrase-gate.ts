// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a route guard renders, decided in one place: the steps that come
 * before any route. New recovery words (after the account keys were changed
 * on this device) are shown and acknowledged first; then the sign-up's
 * "Protect your account" step (ADR-0075, `docs/ux/identity-recovery.md`
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
 * ADR-0075 D2: the recovery phrase is NOT shown at sign-up. The phrase
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
  /** ADR-0075 (UX 19.1a, 19.1c): the "Protect your account" step: after
   *  sign-up, and at every landing of an account with a username and no
   *  second way in (`protect/protect-step.ts`, `protectStepShown`). */
  | "protect-account"
  /** docs/ux/identity-recovery.md 7.1 — a forgot-password route succeeded
   *  on a device that holds the keys: choose a new password, or "Not now". */
  | "choose-new-password"
  /** A guest account (`auth/guest-account.ts`): the way back to its call, or
   *  the question whether to keep the account. Never the app itself. */
  | "guest"
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

/**
 * The guard around protected routes (`<RequireSession>`).
 *
 * `guest`: the session is a guest account. It has no conversations and no
 * contacts, so the app behind the guard is an empty shell and the guest never
 * lands in it. Owed recovery words still come first. A guest that keeps its
 * account (a username and a first password, `GuestAccountOffer`) is not shown
 * a phrase: its marker goes and the "Protect your account" step follows.
 */
export function sessionGuardView(
  kind: GuardAuthKind,
  signupPhrasePending: boolean,
  newPasswordOffered = false,
  protectStepOwed = false,
  guest = false,
  /** UX 19.1c (item G): the landing still waits for the factor read. */
  factorsPending = false,
): GuardView {
  // Owed new words still come first. Then the guest screen, before the
  // steps of a kept account: a guest that chose to keep its account leaves
  // this branch the moment its marker is cleared, and lands on "Protect
  // your account" (which is marked owed before the marker goes).
  if (kind === "session" && guest && !signupPhrasePending) return "guest";
  const shared = sharedView(kind, signupPhrasePending, newPasswordOffered, protectStepOwed);
  if (shared !== null) return shared;
  if (kind !== "session") return "redirect-login";
  // The app waits for the factor read (at most a few seconds), so the
  // "Protect your account" step never replaces it a moment after it shows.
  return factorsPending ? "loading" : "children";
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

// ---------------------------------------------------------------------------
// A new identity whose phrase screen would cover another account
// ---------------------------------------------------------------------------

/** What happens to a freshly minted recovery phrase once its identity has
 *  been published. */
export type FreshPhraseDisposition =
  /** Show the "save your phrase" screen now. */
  | "show"
  /** Leave the screen as it is; the minting account's next sign-in on this
   *  browser shows the phrase first. */
  | "owe";

/**
 * ADR-0017 lost-everything fallback, after the publish. The publish cannot be
 * taken back, so the phrase is shown whenever this browser is still the
 * minting account's or nobody's: the binding unchanged, a sign-out, the same
 * account parked again. When ANOTHER account is bound (signed in, or sitting
 * in its own restore / new-phrase prompt), its screen is not replaced: the
 * phrase is already in the minting account's device-protected cache and stays
 * owed to that account.
 *
 * `boundAccount` is the account id the auth state names, `null` when it names
 * none (`anon`, `loading`).
 */
export function freshPhraseDisposition(args: {
  readonly mintedFor: string;
  readonly boundAccount: string | null;
}): FreshPhraseDisposition {
  if (args.boundAccount === null) return "show";
  return args.boundAccount === args.mintedFor ? "show" : "owe";
}

/**
 * Whether a sign-in that would park in the restore prompt (no readable
 * AccountIdentity seed on this device) is completed from the phrase this
 * device kept instead. Only for an account whose phrase screen is still owed:
 * that phrase was never shown, so the person cannot type it. And only with a
 * device key, which is what the kept phrase is wrapped under. The completion
 * itself is the ordinary phrase path, which refuses a phrase the published
 * identity does not confirm.
 */
export function owedPhraseCompletesPark(args: {
  readonly owed: boolean;
  readonly deviceKeyPresent: boolean;
}): boolean {
  return args.owed && args.deviceKeyPresent;
}

// ---------------------------------------------------------------------------
// A phrase the person typed
// ---------------------------------------------------------------------------

/** Where the phrase a landing completes with came from. */
export type PhraseSource =
  /** The person typed it (the restore prompt, "Recover with the phrase"). */
  | "typed"
  /** This device's own row: the phrase a fresh identity still owes its
   *  screen (`completeParkFromOwedPhrase`), or a guest's own phrase. */
  | "device"
  /** Opened from a vault the person never saw the words of: a passkey's
   *  (the recovery email's vault was removed on 2026-11-26). */
  | "vault";

/**
 * ADR-0077 decision 3 rule 6: a phrase the person typed clears the owed
 * phrase screen, since they have shown they hold it; a phrase that came from
 * this device or a vault does not, so a phrase never shown is still shown
 * (the Android `phraseHeldByPerson`).
 */
export function phraseSourceClearsOwed(source: PhraseSource): boolean {
  return source === "typed";
}
