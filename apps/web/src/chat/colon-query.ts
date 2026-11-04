// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/reactions-and-emoji.md` section 13 — the `:` suggestions: the
 * `:word` the caret stands at the end of. `:` must start a word (the text's
 * start or after white space) and be followed by two or more letters; the
 * caret must be at the end of the word. Pure, so it can be tested.
 */
export interface ColonQuery {
  /** Index of the `:`. */
  readonly start: number;
  /** Index just past the word (the caret). */
  readonly end: number;
  /** The letters after the `:`. */
  readonly word: string;
}

export function colonQueryAt(text: string, caret: number): ColonQuery | null {
  if (caret < 0 || caret > text.length) return null;
  let i = caret;
  while (i > 0 && /[\p{L}\p{N}_]/u.test(text[i - 1] ?? "")) i -= 1;
  if (i === 0 || text[i - 1] !== ":") return null;
  const colon = i - 1;
  if (colon > 0 && !/\s/u.test(text[colon - 1] ?? "")) return null;
  const word = text.slice(i, caret);
  if ([...word].filter((c) => /\p{L}/u.test(c)).length < 2) return null;
  return { start: colon, end: caret, word };
}

/** `text` with the query replaced by `emoji` and a caret just after it. */
export function replaceColonQuery(text: string, q: ColonQuery, emoji: string): { text: string; caret: number } {
  const next = text.slice(0, q.start) + emoji + text.slice(q.end);
  return { text: next, caret: q.start + emoji.length };
}
