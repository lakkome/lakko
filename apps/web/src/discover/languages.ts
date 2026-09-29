// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The languages Discover offers (ADR-0059: ISO 639 codes, 2-3 lowercase
 * letters). The list is a chosen set of common languages; the app language
 * is added when it is not among them. Labels come from the browser's
 * `Intl.DisplayNames` in the language itself, else the code.
 */

export const LANGUAGE_CODES: readonly string[] = [
  "en", "es", "pt", "fr", "de", "it", "nl", "pl", "uk", "ru", "tr", "ar",
  "fa", "hi", "id", "zh", "ja", "ko",
];

const LANG_RE = /^[a-z]{2,3}$/;

/** The app language as an ISO 639 code, `en` when the browser says nothing
 *  usable. */
export function appLanguage(navigatorLanguage: string | undefined): string {
  const code = (navigatorLanguage ?? "").toLowerCase().split(/[-_]/)[0] ?? "";
  return LANG_RE.test(code) ? code : "en";
}

/** The offered list, the given languages first when missing from it. */
export function languageOptions(...extra: readonly (string | null | undefined)[]): string[] {
  const out = [...LANGUAGE_CODES];
  for (const e of extra) if (e !== null && e !== undefined && LANG_RE.test(e) && !out.includes(e)) out.unshift(e);
  return out;
}

/** "English", "Deutsch", ... in the language itself; the code as a fallback. */
export function languageLabel(code: string): string {
  try {
    const names = new Intl.DisplayNames([code], { type: "language" });
    const name = names.of(code);
    if (name !== undefined && name !== code) return name.charAt(0).toUpperCase() + name.slice(1);
  } catch {
    // An engine without DisplayNames, or an unknown code.
  }
  return code;
}
