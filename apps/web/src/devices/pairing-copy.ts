// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/second-device.md section 2.1: the lines that name the device being
 * added, by what its pairing payload's card says about it. With no name (no
 * card, a card that did not decode, an empty name) the unnamed lines stay.
 * The card is what the new device says about itself and nothing checks it;
 * the warning under the title stays.
 */
import { t } from "../l10n";

/** The confirm step's title after a code was scanned. */
export function pairingConfirmTitle(name: string | null): string {
  return name === null
    ? t("devices.scan.confirm.title")
    : t("devices.scan.confirm.title_named", { name });
}

/** The line once a scanned device was added. */
export function pairingScanDone(name: string | null): string {
  return name === null
    ? t("settings.devices.scan.done")
    : t("settings.devices.scan.done_named", { name });
}
