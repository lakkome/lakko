// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The confirmation step of the recovery-phrase screen
 * (docs/ux/client-redesign.md section 5.6): after the 12 words are shown,
 * the user re-enters three of them at positions chosen at random, so a
 * phrase that was never written down is caught before Done.
 *
 * Client-side only. Nothing here talks to the server; the positions picked
 * here are drawn at random for this screen, so the person does not learn
 * to keep only three words.
 *
 * Pure: vitest runs in plain node without the Solid transform, so the
 * picker and the checker live here, apart from the screen that renders them.
 */

import { currentLanguage, t } from "../l10n";

/**
 * Word positions as a list ("2, 7 and 11"). Positions stay Latin digits in
 * every language (ADR-0061 open question 2); only the separators and the
 * conjunction follow the language.
 */
export function listPositions(positions: readonly number[]): string {
  const items = positions.map(String);
  if (items.length <= 1) return items.join("");
  const language = currentLanguage();
  if (language !== "en") {
    try {
      return new Intl.ListFormat(language, { type: "conjunction" }).format(items);
    } catch {
      // No list data for the language: fall through to the English form.
    }
  }
  return `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}`;
}

/** How many words the user re-enters. */
export const CONFIRM_WORDS = 3;

/** The words of a phrase as typed or shown: split on any whitespace. */
export function phraseWords(phrase: string): string[] {
  return phrase.split(/\s+/).filter((w) => w.length > 0);
}

/**
 * Pick `count` distinct 1-based positions out of `wordCount`, sorted
 * ascending so the slots read in phrase order. `random` is `Math.random`
 * by default and injectable for a deterministic test.
 *
 * Fewer words than `count` yields every position there is; zero words
 * yields nothing.
 */
export function pickConfirmationPositions(
  wordCount: number,
  count = CONFIRM_WORDS,
  random: () => number = Math.random,
): number[] {
  const total = Math.max(0, Math.floor(wordCount));
  const wanted = Math.min(Math.max(0, Math.floor(count)), total);
  // Partial Fisher-Yates over 1..total: `wanted` draws, each distinct.
  const pool = Array.from({ length: total }, (_, i) => i + 1);
  for (let i = 0; i < wanted; i += 1) {
    const r = Math.min(Math.max(random(), 0), 0.999_999_999);
    const j = i + Math.floor(r * (total - i));
    const tmp = pool[i] as number;
    pool[i] = pool[j] as number;
    pool[j] = tmp;
  }
  return pool.slice(0, wanted).sort((a, b) => a - b);
}

/** A typed word, normalised the way the phrase itself is compared: trimmed,
 *  lower-case. Nothing else is forgiven; a BIP39 word has no other form. */
export function normaliseTypedWord(word: string): string {
  return word.trim().toLowerCase();
}

/**
 * Which of the asked positions do not match the phrase. `typed[i]` is the
 * answer for `positions[i]`. Empty when every word matches; a missing or
 * blank answer counts as a mismatch, so an incomplete form never passes.
 */
export function confirmationMismatches(
  words: readonly string[],
  positions: readonly number[],
  typed: readonly string[],
): number[] {
  const out: number[] = [];
  positions.forEach((pos, i) => {
    const expected = words[pos - 1];
    const got = normaliseTypedWord(typed[i] ?? "");
    if (expected === undefined || got.length === 0 || got !== expected) {
      out.push(pos);
    }
  });
  return out;
}

/** Whether every asked position has a non-blank answer, so a check is worth
 *  running (the Done button's enabled state). */
export function confirmationComplete(
  positions: readonly number[],
  typed: readonly string[],
): boolean {
  return (
    positions.length > 0 &&
    positions.every((_, i) => normaliseTypedWord(typed[i] ?? "").length > 0)
  );
}

/** The one-sentence error for a failed check: names the first wrong
 *  position and says what to do. */
export function confirmationErrorMessage(mismatches: readonly number[]): string | null {
  const first = mismatches[0];
  if (first === undefined) return null;
  // Positions stay Latin digits in every language (ADR-0061 open question
  // 2), so they are passed as text, not as numbers.
  return mismatches.length === 1
    ? t("recovery_phrase.check.mismatch_one", { position: String(first) })
    : t("recovery_phrase.check.mismatch_many", { positions: mismatches.join(", ") });
}
