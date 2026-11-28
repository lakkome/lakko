// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "The last one cannot be removed" (`docs/ux/identity-recovery.md` 19.1c):
 * removing the recovery email or the last passkey is refused when the
 * account would be left with no second way in. The app's own dialog says so
 * before any request when the facts already show it, and again when the
 * server answers `409 last-factor` (the facts were stale, or not known).
 */
import { t } from "../../l10n";
import { requestConfirm, type ConfirmRequest } from "../../ui/confirm-dialog";
import { removingLeavesNone, type AccountFactors, type RemovableFactor } from "./factors";

/** The dialog: the surface's name as its title, the one line, `OK`. */
export function lastFactorNotice(which: RemovableFactor): ConfirmRequest {
  return {
    title: which === "email" ? t("recovery_email.title") : t("settings.passkeys.title"),
    body: t("protect.last_factor"),
    confirmLabel: t("common.ok"),
    cancelLabel: t("common.cancel"),
    tone: "neutral",
    notice: true,
  };
}

/** Show the dialog. Resolves when it was closed. */
export async function showLastFactorNotice(which: RemovableFactor): Promise<void> {
  await requestConfirm(lastFactorNotice(which));
}

/**
 * The check before a removal: `true` when the facts say `which` is the
 * account's last second way in (the dialog was shown, nothing is to be
 * sent). Unknown facts refuse nothing here: the server decides.
 */
export async function refusedAsLastFactor(
  factors: AccountFactors | null,
  which: RemovableFactor,
): Promise<boolean> {
  if (factors === null || !removingLeavesNone(factors, which)) return false;
  await showLastFactorNotice(which);
  return true;
}
