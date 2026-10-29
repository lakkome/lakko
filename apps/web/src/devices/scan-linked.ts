// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/second-device.md section 2.1, "When it is done", for the scan
 * route: once `Link device` finished, the screen goes straight back to the
 * Devices list, the state before `Add a device` was pressed, with the list
 * read again so the new device's row shows its name from its card. There is
 * no "added" line and no OK button. The one line that stays is the existing
 * keys-not-sent one, when this device handed the new one no keys.
 *
 * A module rather than a condition in the JSX, so the rule is host-tested
 * (vitest runs without a DOM or the Solid transform).
 */

/** Where the Devices screen goes after a scanned device was linked. */
export interface ScanLinkedNext {
  /** The add panel closes: back to the list. */
  readonly add: "closed";
  /** The list is read again, so the new row appears. */
  readonly reloadList: true;
  /** Show the existing keys-not-sent line under the list. */
  readonly keysNotSent: boolean;
}

/** The screen after a successful scan-route link. `recoveryPublished`: the
 *  new device was handed a session and the recovery phrase. */
export function afterScanLinked(result: { readonly recoveryPublished: boolean }): ScanLinkedNext {
  return { add: "closed", reloadList: true, keysNotSent: !result.recoveryPublished };
}
