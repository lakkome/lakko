// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/second-device.md section 2.1: the confirm title that names the
 * device being added, by what its pairing payload's card says about it. With
 * no name (no card, a card that did not decode, an empty name) the unnamed
 * title stays. Once linked there is no result line (section 2.1, "When it is
 * done").
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
