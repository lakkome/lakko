// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Your chats depend on your password" (`docs/ux/identity-recovery.md`
 * 19.1d, ADR-0101 section 10): when the notice applies and where it is
 * shown. Pure.
 *
 * After a key reset, contacts give the chats back only when the reset is
 * confirmed by something the account set up before: the password as it is
 * now, a passkey that keeps the chats, or the recovery phrase. An account is
 * kept by the password only when it has a second way in but neither a saved
 * phrase nor such a passkey.
 */
import { chatsKeptBy, type OwnSealFacts } from "../seal/own-seals";
import { hasSecondFactor, type AccountFactors } from "./factors";

/**
 * Whether the notice applies. Unknown facts show nothing: the notice is
 * never drawn on a guess, in either direction.
 */
export function passwordOnlyNoticeApplies(
  factors: AccountFactors | null,
  seals: OwnSealFacts | null,
): boolean {
  return factors !== null && hasSecondFactor(factors) && chatsKeptBy(seals) === "password";
}

/** The three places of the notice. */
export type PasswordOnlyPlace = "step" | "settings-recovery" | "dialog";

/**
 * Whether the notice is shown in `place`.
 *
 *   - `step` and `settings-recovery`: for as long as it applies; it cannot
 *     be dismissed there.
 *   - `dialog`: once on a device, the first time the app opens after a
 *     sign-in there. Not while a step that comes before every route is up,
 *     and never again once it was seen (shown and closed by either button,
 *     or made unnecessary: the sign-up step and a key reset on this device
 *     already said what is kept).
 */
export function passwordOnlyShownIn(
  place: PasswordOnlyPlace,
  args: {
    readonly applies: boolean;
    /** `dialog` only: the per-device "seen" marker. */
    readonly dialogSeen?: boolean;
    /** `dialog` only: a guard step (new words, the protect step, the
     *  new-password step) or the signed-out screen is up. */
    readonly guardStepUp?: boolean;
  },
): boolean {
  if (!args.applies) return false;
  if (place !== "dialog") return true;
  return args.dialogSeen !== true && args.guardStepUp !== true;
}

/** Whether `Add a passkey` is on the notice: only where this device or
 *  browser can create passkeys, for an account with a username. */
export function passwordOnlyOffersPasskey(passkeysSupported: boolean, hasUsername: boolean): boolean {
  return passkeysSupported && hasUsername;
}
