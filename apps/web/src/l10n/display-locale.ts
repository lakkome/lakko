// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The locale every displayed time, date, relative time, number, file size and
 * duration is formatted with (`docs/ux/localization.md` section 5, "The
 * language decides words, the device decides formats", 2026-10-29).
 *
 * The interface language supplies the words: month and weekday names, the
 * words of relative times, the AM/PM marker when one is shown. The device
 * supplies everything else: the 12- or 24-hour clock, the order of day, month
 * and year, and the decimal and grouping separators. A browser does not expose
 * the operating system's clock setting, so "the device" is the browser's own
 * default locale (`navigator.language`, else the `Intl` default).
 *
 * The result is the interface language, with its script, carrying the
 * device's region and the device's hour cycle (`en` + `de-DE` gives
 * `en-DE-u-hc-h23`). Where CLDR has no data for that pair the engine falls
 * back to the language's own field order, but the hour cycle is applied
 * either way, so the clock always follows the device.
 *
 * Fixed renderings (safety numbers, recovery phrase positions) do not go
 * through this and keep their Latin digits.
 */
import { currentLanguage } from "./index";
import { deviceLocale, formatLocaleFor } from "./format-locale";

export { deviceLocale, formatLocaleFor } from "./format-locale";

/** The formatting locale now: the current interface language on this
 *  browser. Reactive through `currentLanguage()`. */
export function displayLocale(): string {
  return formatLocaleFor(currentLanguage(), deviceLocale());
}
