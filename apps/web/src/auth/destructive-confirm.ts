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

import { type ConfirmRequest } from "../ui/confirm-dialog";
import { t } from "../l10n";

// The copy lives in the catalog (ADR-0061) and is rendered when the dialog
// is requested, so it is in the language current at that moment.

// ---------------------------------------------------------------------------
// Remove a passkey
// ---------------------------------------------------------------------------

/** Generic per §6's shape — the passkey is named on the button, not here. */
export const REMOVE_PASSKEY_TITLE_KEY = "settings.passkeys.remove.title";

/** Carried over from the string this replaced, with the irreversibility
 *  sentence moved to the end. */
export const REMOVE_PASSKEY_BODY_KEY = "settings.passkeys.remove.body";

/** The destructive action: the bare verb, or the verb naming the passkey. */
export const REMOVE_PASSKEY_CONFIRM_KEY = "settings.passkeys.remove.confirm";
export const REMOVE_PASSKEY_CONFIRM_NAMED_KEY = "settings.passkeys.remove.confirm_named";

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
  const nickname = typeof input.nickname === "string" ? input.nickname.trim() : "";
  return {
    title: t(REMOVE_PASSKEY_TITLE_KEY),
    body: t(REMOVE_PASSKEY_BODY_KEY),
    confirmLabel:
      nickname.length === 0
        ? t(REMOVE_PASSKEY_CONFIRM_KEY)
        : t(REMOVE_PASSKEY_CONFIRM_NAMED_KEY, { name: nickname }),
    cancelLabel: t("common.cancel"),
    tone: "danger",
  };
}

// ---------------------------------------------------------------------------
// Stop a passkey restoring keys
// ---------------------------------------------------------------------------

/** The question. */
export const STOP_RESTORING_TITLE_KEY = "settings.passkeys.stop_restoring.title";
/** The consequence (docs/ux/identity-recovery.md 10.2). */
export const STOP_RESTORING_BODY_KEY = "settings.passkeys.stop_restoring.body";
/** The action. */
export const STOP_RESTORING_CONFIRM_KEY = "settings.passkeys.stop_restoring.action";

/**
 * "Stop restoring keys" for one passkey. Not destructive to the account:
 * the passkey keeps signing in and keys already restored elsewhere stay, so
 * the tone is plain. What is removed is what the server stored for that
 * passkey.
 */
export function stopRestoringKeysConfirmation(): ConfirmRequest {
  return {
    title: t(STOP_RESTORING_TITLE_KEY),
    body: t(STOP_RESTORING_BODY_KEY),
    confirmLabel: t(STOP_RESTORING_CONFIRM_KEY),
    cancelLabel: t("common.cancel"),
    tone: "neutral",
  };
}

// ---------------------------------------------------------------------------
// Forget this device
// ---------------------------------------------------------------------------

/** The question. */
export const FORGET_DEVICE_TITLE_KEY = "settings.account.forget_device.title";

/** Carried over verbatim. The second sentence is the one fact that decides
 *  whether to press the button, so it is not summarised. */
export const FORGET_DEVICE_BODY_KEY = "settings.account.forget_device.body";

/**
 * The action.
 *
 * A singleton target — there is one device being forgotten and the user is on
 * it — so the noun names it and an identifier would add nothing. §6.1's rule
 * is that the button says what it acts on; `Forget device` does, `Forget`
 * alone does not.
 */
export const FORGET_DEVICE_CONFIRM_KEY = "settings.account.forget_device.confirm";

/** The confirmation for wiping this browser's cached account. */
export function forgetDeviceConfirmation(): ConfirmRequest {
  return {
    title: t(FORGET_DEVICE_TITLE_KEY),
    body: t(FORGET_DEVICE_BODY_KEY),
    confirmLabel: t(FORGET_DEVICE_CONFIRM_KEY),
    cancelLabel: t("common.cancel"),
    tone: "danger",
  };
}

// ---------------------------------------------------------------------------
// Delete the account
// ---------------------------------------------------------------------------

/** The question. */
export const DELETE_ACCOUNT_TITLE_KEY = "settings.account.delete.title";

/**
 * The body — enumerated rather than sweeping, on §6.2.4's rule: the user
 * cannot check afterwards, so the confirmation is where the scope is stated.
 * This one spans both tiers (the server-side identity AND this device) and
 * says so, because "delete your account" alone would not tell the user their
 * local messages go too.
 */
export const DELETE_ACCOUNT_BODY_KEY = "settings.account.delete.body";

/** The action. A singleton, named by its noun for the same reason as
 *  {@link FORGET_DEVICE_CONFIRM_KEY}. */
export const DELETE_ACCOUNT_CONFIRM_KEY = "settings.account.delete.confirm";

/** The confirmation for the A3 (GDPR right-to-erasure) account deletion. */
export function deleteAccountConfirmation(): ConfirmRequest {
  return {
    title: t(DELETE_ACCOUNT_TITLE_KEY),
    body: t(DELETE_ACCOUNT_BODY_KEY),
    confirmLabel: t(DELETE_ACCOUNT_CONFIRM_KEY),
    cancelLabel: t("common.cancel"),
    tone: "danger",
  };
}
