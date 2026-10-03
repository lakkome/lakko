// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "This device was removed" (docs/ux/second-device.md section 3.1).
 *
 * Another device of the account removed this one: once this device applies
 * that commit its DeviceGroup is inactive (`DeviceGroup.isActive` is
 * `false`), frozen for good, and nothing new decrypts. The sync runtime then
 * stops polling, persists nothing more, and the app shell shows one
 * full-screen state instead of the app.
 *
 * The decisions live here, apart from the Solid runtime, so they can be
 * tested without a Delivery Service.
 */

import { t } from "../../l10n";

/** The copy of the removed-device screen, exactly as the UX spec words it,
 *  rendered in the current language on every call. */
export const REMOVED_DEVICE_COPY = {
  title: (): string => t("devices.removed.title"),
  body: (): string => t("devices.removed.body"),
  signOut: (): string => t("devices.removed.sign_out"),
} as const;

/** The copy of the signed-out screen after a key reset on another device
 *  (docs/ux/identity-recovery.md section 15.2, ADR-0065). The two body
 *  lines that name the account are rendered with `tRich` from their keys
 *  (`devices.signed_out.body_removing` / `body_removed`). */
export const SIGNED_OUT_COPY = {
  title: (): string => t("devices.signed_out.title"),
  removing: (): string => t("devices.signed_out.removing"),
  bodyMismatch: (): string => t("devices.signed_out.body_mismatch"),
  signIn: (): string => t("devices.signed_out.sign_in"),
  thisAccount: (): string => t("devices.signed_out.this_account"),
} as const;

/** What the runtime reads: the wasm DeviceGroup's `isActive` getter. */
export interface ActiveReadable {
  readonly isActive: boolean;
}

/**
 * Whether the DeviceGroup handle reports this device still in the account's
 * DeviceGroup; `null` when the read itself fails (a poisoned lock, a freed
 * handle). A failed read is not proof of removal.
 */
export function readDeviceGroupActive(g: ActiveReadable): boolean | null {
  try {
    return g.isActive;
  } catch {
    return null;
  }
}

/** Only an explicit `false` is a removal: the screen must never show on a
 *  read that failed. */
export function isDeviceRemoved(active: boolean | null): boolean {
  return active === false;
}

/** What the poll loop does after applying a batch (or after loading a
 *  handle): stop for good once the device is removed, otherwise go on. */
export function pollStepAfterBatch(active: boolean | null): "stop-removed" | "continue" {
  return isDeviceRemoved(active) ? "stop-removed" : "continue";
}

/**
 * Whether the ADR-0057 fork repair may run on this handle. The repair resets
 * the account's DeviceGroup chain, so it runs only on a group known to be
 * active: never on a removed device (it is not forked, it is out), and not on
 * a handle whose state could not be read.
 */
export function forkRepairAllowed(active: boolean | null): boolean {
  return active === true;
}
