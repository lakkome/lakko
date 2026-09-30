// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A call's length said in words, for a screen reader (`docs/ux/calls.md` §8)
 * and the call history, in the interface language (ADR-0058).
 *
 * Each unit is its own plural message (`common.duration.hours` / `minutes` /
 * `seconds`), and the units present are joined pair by pair through
 * `common.duration.pair`, so no unit word and no word order is written here.
 */
import { t } from "../l10n";

/**
 * The words for a whole number of seconds. Hours and minutes appear only when
 * they are not zero; seconds always appear when `alwaysSeconds` is set,
 * otherwise only when they are not zero or nothing else would be said.
 */
export function durationWords(totalSeconds: number, alwaysSeconds: boolean): string {
  const total = Math.max(0, Math.floor(totalSeconds));
  const seconds = total % 60;
  const minutes = Math.floor(total / 60) % 60;
  const hours = Math.floor(total / 3600);
  const parts: string[] = [];
  if (hours > 0) parts.push(t("common.duration.hours", { count: hours }));
  if (minutes > 0) parts.push(t("common.duration.minutes", { count: minutes }));
  if (alwaysSeconds || seconds > 0 || parts.length === 0) {
    parts.push(t("common.duration.seconds", { count: seconds }));
  }
  return parts.reduce(
    (acc, part) => (acc.length === 0 ? part : t("common.duration.pair", { first: acc, second: part })),
    "",
  );
}
