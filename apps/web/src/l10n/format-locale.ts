// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The pure half of `display-locale.ts`: which locale a formatter uses, given
 * the interface language and the device's own locale. No imports, so the
 * l10n runtime can use it for the numbers inside catalog messages.
 */

/** The browser's own default locale, or `undefined` when none is readable. */
export function deviceLocale(): string | undefined {
  try {
    if (typeof navigator !== "undefined" && typeof navigator.language === "string" && navigator.language !== "") {
      return navigator.language;
    }
  } catch {
    // fall through to the engine default
  }
  try {
    return new Intl.DateTimeFormat().resolvedOptions().locale;
  } catch {
    return undefined;
  }
}

/** The hour cycle a locale's clock uses (`h23`, `h12`, ...), or `undefined`. */
function hourCycleOf(locale: string): string | undefined {
  try {
    return new Intl.DateTimeFormat(locale, { hour: "numeric" }).resolvedOptions().hourCycle;
  } catch {
    return undefined;
  }
}

/**
 * The formatting locale for `interfaceLanguage` on a device whose own locale
 * is `device`. Pure apart from the engine's locale data; any input the engine
 * refuses falls back to the interface language alone.
 */
export function formatLocaleFor(interfaceLanguage: string, device: string | undefined): string {
  if (device === undefined || device === "") return interfaceLanguage;
  try {
    const words = new Intl.Locale(interfaceLanguage);
    const formats = new Intl.Locale(device);
    const region = formats.region ?? formats.maximize().region ?? words.region;
    const hourCycle = hourCycleOf(device);
    const options: Intl.LocaleOptions = {};
    if (words.script !== undefined) options.script = words.script;
    if (region !== undefined) options.region = region;
    if (hourCycle === "h11" || hourCycle === "h12" || hourCycle === "h23" || hourCycle === "h24") {
      options.hourCycle = hourCycle;
    }
    const tag = new Intl.Locale(words.language, options).toString();
    // Proves the engine accepts the tag before anyone formats with it.
    new Intl.DateTimeFormat(tag);
    return tag;
  } catch {
    return interfaceLanguage;
  }
}
