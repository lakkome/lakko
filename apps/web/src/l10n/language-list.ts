// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The Settings > Language list (`docs/ux/localization.md` section 3): rows
 * sorted by the language name in the current interface language, filtered
 * by a search that matches the own name or the translated name, ignoring
 * case and diacritics. Kept out of the `.tsx` so it is testable in node.
 */
import type { LanguageInfo } from "./runtime";

/** One row of the list. */
export interface LanguageRow {
  readonly code: string;
  /** The row title: the language written in itself. */
  readonly ownName: string;
  /** The row subtitle: the language's name in the current UI language. */
  readonly translatedName: string;
}

/** The catalog key of a language's translated name. */
export function languageNameKey(code: string): string {
  return `language.name.${code.toLowerCase()}`;
}

/** Lowercase with diacritics removed, for matching. */
export function foldForSearch(s: string, locale: string): string {
  return s
    .normalize("NFD")
    .replace(/\p{M}/gu, "")
    .toLocaleLowerCase(locale);
}

/** Build, filter and sort the rows. */
export function languageRows(
  languages: readonly LanguageInfo[],
  translate: (key: string) => string,
  query: string,
  locale: string,
): LanguageRow[] {
  const rows = languages.map((l) => ({
    code: l.code,
    ownName: l.ownName,
    translatedName: translate(languageNameKey(l.code)),
  }));
  const q = foldForSearch(query.trim(), locale);
  const matching =
    q.length === 0
      ? rows
      : rows.filter(
          (r) =>
            foldForSearch(r.ownName, locale).includes(q) ||
            foldForSearch(r.translatedName, locale).includes(q),
        );
  const collator = new Intl.Collator(locale);
  return matching.sort((a, b) => collator.compare(a.translatedName, b.translatedName));
}

/** Whether the "Same as system" row survives the search. */
export function systemRowMatches(label: string, query: string, locale: string): boolean {
  const q = foldForSearch(query.trim(), locale);
  return q.length === 0 || foldForSearch(label, locale).includes(q);
}
