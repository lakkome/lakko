// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The copy of the remove-device sheet and of the removed-device screen
 * (`docs/ux/identity-recovery.md` sections 18.1, 18.2 and 18.6).
 */
import { t } from "../l10n";
import type { RevokeFailure } from "./removal";

export const REMOVE_DEVICE_COPY = {
  title: (device: string): string => t("settings.devices.remove.title", { device }),
  body: (): string => t("settings.devices.remove.body"),
  confirm: (): string => t("settings.devices.remove.confirm"),
  working: (): string => t("settings.devices.remove.working"),
  /** The row label while the removal is still finishing. */
  pending: (): string => t("devices.remove.pending"),
  /** The row's button when the owed step waits on the person's passkey. */
  finish: (): string => t("devices.remove.finish"),
  done: (): string => t("devices.remove.done"),
  notRevoked: (): string => t("devices.remove.error.not_revoked"),
  tryLater: (): string => t("devices.remove.error.try_later"),
  /** A key-reset publish is owed: the existing owed copy (UX 15.4). */
  blocked: (): string => t("recovery_phrase.fresh.publish_owed"),
  cancel: (): string => t("common.cancel"),
} as const;

/** "It was lost or stolen": remove the device AND change the account keys
 *  (UX 18.1, the sheet's second choice and its states). */
export const ROTATE_COPY = {
  reasonUnused: (): string => t("settings.devices.remove.reason_unused"),
  reasonStolen: (): string => t("settings.devices.remove.reason_stolen"),
  title: (): string => t("devices.rotate.title"),
  lede: (): string => t("devices.rotate.lede"),
  /** `newWords`: whether the account gets new words to save (ADR-0067: only
   *  a phrase-protected account; any other is shown none, so the line is
   *  left out). */
  effects: (newWords: boolean): string[] => [
    t("devices.rotate.effect_devices"),
    ...(newWords ? [t("devices.rotate.effect_phrase")] : []),
    t("devices.rotate.effect_passkeys"),
    t("devices.rotate.effect_contacts"),
    // ADR-0066 section 3.5a rule 5: what was granted before is spent.
    t("devices.rotate.effect_links"),
  ],
  confirm: (): string => t("devices.rotate.confirm"),
  stepRemove: (): string => t("devices.rotate.step_remove"),
  stepKeys: (): string => t("devices.rotate.step_keys"),
  stepDevices: (): string => t("devices.rotate.step_devices"),
  done: (): string => t("devices.rotate.done"),
  notStarted: (): string => t("devices.rotate.error.not_started"),
  retry: (): string => t("devices.rotate.retry"),
  notNow: (): string => t("devices.rotate.not_now"),
} as const;

/** The sheet's own line for a revoke that did not go through, or `null`
 *  when the proof block words it (the proof is asked for again). */
export function revokeFailureLine(failure: RevokeFailure): (() => string) | null {
  switch (failure) {
    case "proof-required":
      return null;
    case "account-key-required":
      return REMOVE_DEVICE_COPY.tryLater;
    case "rate-limited":
      return () => t("common.error.too_many_attempts");
    case "unconfirmed":
    case "other":
      return REMOVE_DEVICE_COPY.notRevoked;
  }
}

/** The removed-device screen (UX 18.2). `name` is the `@username`, or the
 *  catalog's "this account" when the device no longer knows it. */
export const REMOVED_SCREEN_COPY = {
  title: (): string => t("devices.removed.title"),
  removing: (): string => t("devices.signed_out.removing"),
  signIn: (): string => t("devices.signed_out.sign_in"),
} as const;
