// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0100 section 4 — how the pack dialog and the settings list name a
 * language group and its size, in the interface language.
 */

import { languageName } from "./model";

/** A size as the dialog shows it: megabytes with one decimal (`147.5 MB`). */
export function formatPackSize(bytes: number, locale: string): string {
  const mb = bytes / 1_000_000;
  try {
    return new Intl.NumberFormat(locale, {
      style: "unit",
      unit: "megabyte",
      unitDisplay: "short",
      maximumFractionDigits: 1,
      minimumFractionDigits: 1,
    }).format(mb);
  } catch {
    return `${mb.toFixed(1)} MB`;
  }
}

/** Language codes as one list in the interface language
 *  (`Spanish, Portuguese and Galician`). */
export function languageListText(codes: readonly string[], locale: string): string {
  const names = codes.map((c) => languageName(c, locale));
  try {
    return new Intl.ListFormat(locale, { style: "long", type: "conjunction" }).format(names);
  } catch {
    return names.join(", ");
  }
}
