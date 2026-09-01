// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The account-lifecycle confirmations — remove a passkey, forget this device,
 * delete the account.
 *
 * ## Why these are here at all
 *
 * They are three of the seven `window.confirm` sites in the web client, and
 * they are the three the ADR-0045 §14 brief did not name — §14 was written
 * about the membership flows, so the account-lifecycle ones were never
 * counted. They are also the most destructive in the app: two of them end the
 * user's access to their own account, and unlike a membership change there is
 * no admin who can undo it afterwards.
 *
 * Leaving them on `window.confirm` while the membership flows moved would
 * have reproduced, inside one client, exactly the asymmetry §6 called a real
 * defect rather than a polish item when it existed across clients.
 *
 * ## Where the copy comes from
 *
 * No spec section owns these strings. Each is carried over from what the app
 * already said — those sentences were reviewed once and are accurate — and
 * split into the title / body / action shape §6 establishes. Two changes were
 * made in the process, both forced by the widget:
 *
 *   - **the target moves out of the question and onto the button.** The old
 *     passkey string was `Remove passkey "laptop"?` because `window.confirm`
 *     renders one string and a browser OK/Cancel pair — there was nowhere
 *     else to put the nickname. With a real action button it belongs on the
 *     button (§6.1: the action button's accessible name is its visible label,
 *     so that is what a screen reader announces at the point of no return),
 *     and the question takes §6's generic `Remove this …?` shape;
 *   - **`You can't undo this` moves to the end of the body**, where §6.2.4
 *     and §6.1.5 both put the irreversibility sentence, instead of leading.
 *
 * The delete-account body is lightly reworded from a parenthetical list to a
 * dashed one; nothing was added or dropped. Marked here because it is copy
 * this module wrote rather than copy it inherited.
 */

import { type ConfirmRequest, namedAction } from "../ui/confirm-dialog";

// ---------------------------------------------------------------------------
// Remove a passkey
// ---------------------------------------------------------------------------

/** Generic per §6's shape — the passkey is named on the button, not here. */
export const REMOVE_PASSKEY_TITLE = "Remove this passkey?";

/** Carried over from the string this replaced, with the irreversibility
 *  sentence moved to the end. */
export const REMOVE_PASSKEY_BODY =
  "The device that owns the matching private key will no longer be able " +
  "to sign in with it. This cannot be undone.";

/** The destructive action's verb; the nickname is appended by
 *  {@link namedAction}. */
export const REMOVE_PASSKEY_CONFIRM_LABEL = "Remove";

/**
 * The confirmation for removing one passkey credential.
 *
 * Note this is the UX gate only. G-13 / ADR-0014's cryptographic gate — a
 * fresh passkey assertion bound to THIS credential's removal — runs after it
 * and is what actually authorises the delete; a dismissed assertion aborts
 * fail-closed regardless of what was answered here.
 */
export function removePasskeyConfirmation(input: {
  /** The credential's user-chosen nickname, or `null` when it has none. */
  readonly nickname: string | null | undefined;
}): ConfirmRequest {
  return {
    title: REMOVE_PASSKEY_TITLE,
    body: REMOVE_PASSKEY_BODY,
    confirmLabel: namedAction(REMOVE_PASSKEY_CONFIRM_LABEL, input.nickname),
    cancelLabel: "Cancel",
    tone: "danger",
  };
}

// ---------------------------------------------------------------------------
// Forget this device
// ---------------------------------------------------------------------------

/** The question. */
export const FORGET_DEVICE_TITLE = "Forget this device?";

/** Carried over verbatim. The second sentence is the one fact that decides
 *  whether to press the button, so it is not summarised. */
export const FORGET_DEVICE_BODY =
  "You'll lose the cached account on this browser. The only way back is " +
  "the 24-word recovery passphrase you wrote down at signup.";

/**
 * The action.
 *
 * A singleton target — there is one device being forgotten and the user is on
 * it — so the noun names it and an identifier would add nothing. §6.1's rule
 * is that the button says what it acts on; `Forget device` does, `Forget`
 * alone does not.
 */
export const FORGET_DEVICE_CONFIRM_LABEL = "Forget device";

/** The confirmation for wiping this browser's cached account. */
export function forgetDeviceConfirmation(): ConfirmRequest {
  return {
    title: FORGET_DEVICE_TITLE,
    body: FORGET_DEVICE_BODY,
    confirmLabel: FORGET_DEVICE_CONFIRM_LABEL,
    cancelLabel: "Cancel",
    tone: "danger",
  };
}

// ---------------------------------------------------------------------------
// Delete the account
// ---------------------------------------------------------------------------

/** The question. */
export const DELETE_ACCOUNT_TITLE = "Delete your account?";

/**
 * The body — enumerated rather than sweeping, on §6.2.4's rule: the user
 * cannot check afterwards, so the confirmation is where the scope is stated.
 * This one spans both tiers (the server-side identity AND this device) and
 * says so, because "delete your account" alone would not tell the user their
 * local messages go too.
 */
export const DELETE_ACCOUNT_BODY =
  "This erases your server-side identity — username, keys, recovery " +
  "factors and backup — and signs you out everywhere. Your local messages " +
  "are wiped from this device too. This cannot be undone.";

/** The action. A singleton, named by its noun for the same reason as
 *  {@link FORGET_DEVICE_CONFIRM_LABEL}. */
export const DELETE_ACCOUNT_CONFIRM_LABEL = "Delete account";

/** The confirmation for the A3 (GDPR right-to-erasure) account deletion. */
export function deleteAccountConfirmation(): ConfirmRequest {
  return {
    title: DELETE_ACCOUNT_TITLE,
    body: DELETE_ACCOUNT_BODY,
    confirmLabel: DELETE_ACCOUNT_CONFIRM_LABEL,
    cancelLabel: "Cancel",
    tone: "danger",
  };
}
