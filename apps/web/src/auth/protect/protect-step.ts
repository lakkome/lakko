// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The "Protect your account" step (ADR-0075 D9, `docs/ux/identity-recovery.md`
 * 19.1a and 19.1c): which rows it offers, which are done, when `Continue`
 * works, and when the step is shown at all. Pure.
 *
 * A second way to confirm it is you is REQUIRED: a recovery email, an
 * authenticator app or a passkey. The recovery phrase is an optional extra
 * on the step and does not count. There is no way off the step but
 * `Continue`, and `Continue` needs one of the three.
 */
import { hasSecondFactor, totpOnlyWarningKey, type AccountFactors } from "./factors";

/** One of the three required ways. */
export type ProtectFactorRow = "email" | "totp" | "passkey";
/** Every row of the step: the three, then the optional phrase. */
export type ProtectChoice = ProtectFactorRow | "phrase";

/**
 * The required rows, in the fixed order email, authenticator, passkey.
 *
 *   - `Add an email` is shown when mail is set up on this server and the
 *     account has a username (the stored value is made from the username).
 *     While that is not known yet the row is absent.
 *   - `Use an authenticator app` is always shown: the server refuses to
 *     start without the authenticator configured.
 *   - `Create a passkey` is shown where this device or browser can use
 *     passkeys and the account has a username (a passkey is registered
 *     under it).
 */
export function protectStepRows(
  mailAvailable: boolean | null,
  hasUsername: boolean,
  passkeysSupported: boolean,
): ProtectFactorRow[] {
  const rows: ProtectFactorRow[] = [];
  if (mailAvailable === true && hasUsername) rows.push("email");
  rows.push("totp");
  if (passkeysSupported && hasUsername) rows.push("passkey");
  return rows;
}

/** Which rows are done, and so marked `protect.done`. */
export function protectStepDone(facts: {
  readonly factors: AccountFactors | null;
  /** The account's published protection lists a recovery phrase. */
  readonly phraseSaved: boolean;
}): ProtectChoice[] {
  const done: ProtectChoice[] = [];
  const f = facts.factors;
  if (f !== null) {
    if (f.email) done.push("email");
    if (f.totp) done.push("totp");
    if (f.passkeys > 0) done.push("passkey");
  }
  if (facts.phraseSaved) done.push("phrase");
  return done;
}

/**
 * Whether `Continue` works: at least one of the three is done. A saved
 * phrase alone does not enable it. Unknown factors keep it disabled: the
 * step never lets an account through on a guess.
 */
export function protectStepCanContinue(factors: AccountFactors | null): boolean {
  return factors !== null && hasSecondFactor(factors);
}

/**
 * The authenticator-only note of the step: the catalog key, or `null` when
 * the authenticator is not the only one of the three that is done.
 */
export function protectStepTotpWarning(factors: AccountFactors | null, phraseSaved: boolean): string | null {
  return totpOnlyWarningKey(factors, phraseSaved);
}

/**
 * Whether the step is shown before the app (UX 19.1a, 19.1c).
 *
 *   - Never for an account without a username (a guest account has no step;
 *     `Create account` after a call marks it owed once the name is chosen).
 *   - While the sign-up's marker is owed: shown, whatever is known. The
 *     marker goes only through `Continue`.
 *   - Otherwise at every session landing for an account that has none of
 *     the three: after a reset that deleted its only passkey, after a
 *     recall, or an account made before the rule. While the factors are not
 *     known the step is NOT shown: it never flashes in front of an account
 *     that turns out to have one.
 *   - Once the landing rule has put the step up (`latched`), it stays up
 *     until `Continue`, like the sign-up's: adding the first way in marks
 *     that row Done and shows the authenticator-only note, and the person
 *     may add another before going on. Without the latch the step would
 *     close by itself the moment the factor read saw the new way in.
 */
export function protectStepShown(args: {
  readonly hasUsername: boolean;
  readonly guest: boolean;
  readonly markerOwed: boolean;
  readonly factors: AccountFactors | null;
  /** The landing rule already showed the step in this session and
   *  `Continue` was not pressed since. */
  readonly latched?: boolean;
}): boolean {
  if (!args.hasUsername || args.guest) return false;
  if (args.markerOwed || args.latched === true) return true;
  return args.factors !== null && !hasSecondFactor(args.factors);
}

/** The heading: the sign-up's own while its marker is owed, the required
 *  one when the landing rule put the step up. */
export function protectStepTitleKey(markerOwed: boolean): string {
  return markerOwed ? "protect.title" : "protect.title_required";
}

/**
 * How long a landing keeps the app off the screen while the factors are
 * read (UX 19.1c, alignment of 2026-11-27, item G). Chosen, not measured:
 * one request's worth on an ordinary connection.
 */
export const LANDING_FACTORS_WAIT_MS = 3_000;

/**
 * Whether a landing still waits for the factor read before the app is shown
 * (UX 19.1c): so the step never appears a moment after the app did. Only an
 * account the step can apply to waits, only while its factors are not
 * known, and never past the wait (`waitOver`: the read failed, or
 * {@link LANDING_FACTORS_WAIT_MS} passed; then the app is shown and the
 * step only when a later read answers with no factor).
 */
export function landingWaitsForFactors(args: {
  readonly hasUsername: boolean;
  readonly guest: boolean;
  readonly markerOwed: boolean;
  readonly factors: AccountFactors | null;
  readonly waitOver: boolean;
  readonly latched?: boolean;
}): boolean {
  if (!args.hasUsername || args.guest || args.markerOwed || args.latched === true) return false;
  return args.factors === null && !args.waitOver;
}
