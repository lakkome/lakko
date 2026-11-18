// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Pure helpers for the translation UI: the version a result is keyed on, the
 * reading list, language names, and the languages a picker asks about.
 * No rule lives here: `rules.ts` is the shared one.
 */

/** The person's reading languages: the interface language, then the extras. */
export function readingLanguages(interfaceLanguage: string, extra: readonly string[]): string[] {
  const out = [interfaceLanguage];
  for (const l of extra) if (!out.includes(l)) out.push(l);
  return out;
}

/** A language's name in `displayLanguage`, or the tag itself when the browser has none. */
export function languageName(tag: string, displayLanguage: string): string {
  try {
    const names = new Intl.DisplayNames([displayLanguage], { type: "language", fallback: "code" });
    return names.of(tag) ?? tag;
  } catch {
    return tag;
  }
}

/**
 * The languages a picker asks the browser about. A list of questions, not a
 * claim of support: each entry is offered only after the engine answered
 * something other than `unavailable` for it.
 */
export const CANDIDATE_LANGUAGES: readonly string[] = [
  "ar", "bg", "bn", "cs", "da", "de", "el", "en", "es", "fa", "fi", "fr", "he", "hi", "hr",
  "hu", "id", "it", "ja", "kn", "ko", "lt", "mr", "nl", "no", "pl", "pt", "ro", "ru", "sk",
  "sl", "sv", "ta", "te", "th", "tr", "uk", "vi", "zh", "zh-Hant",
];

/** How many characters a transcript shows before `Show more` (UX 4.3). Chosen. */
export const TRANSCRIPT_PREVIEW_CHARS = 280;

/** How long the "already in English" line stays (UX 3.3). Chosen. */
export const SAME_LANGUAGE_LINE_MS = 4000;

/** The key a message's view state is filed under. */
export function messageKey(slot: string, conversation: string, messageId: number): string {
  return `${slot}|${conversation}|${messageId}`;
}

/**
 * Whether this page can tell a mobile connection from another at all: the
 * browser reports a connection type (Chrome on Android does; desktop
 * browsers and the desktop app's webview do not). Where it cannot,
 * {@link onCellular} is always `false` and a mobile-data setting governs
 * nothing.
 */
export function connectionTypeKnown(): boolean {
  const c = (globalThis.navigator as { connection?: { type?: unknown } } | undefined)?.connection;
  return typeof c?.type === "string" && c.type.length > 0 && c.type !== "unknown";
}

/** Whether the device is on a cellular link, where the browser says. */
export function onCellular(): boolean {
  const c = (globalThis.navigator as { connection?: { type?: string } } | undefined)?.connection;
  return c?.type === "cellular";
}
