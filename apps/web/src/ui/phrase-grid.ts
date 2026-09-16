// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Layout rule for the recovery-phrase grid (docs/ux/client-redesign.md
 * section 5.6): the 24 words read DOWN the first column (1..12) and then
 * down the second (13..24), the way a phrase is written on paper, not
 * across the rows. `PhraseGrid.tsx` renders what this returns; the rule is
 * here so it can be tested without the Solid transform.
 */

/** One numbered word in the grid. `position` is 1-based. */
export interface NumberedWord {
  readonly position: number;
  readonly word: string;
}

/**
 * Split `words` into `columns` runs of consecutive positions, column-major.
 * The first columns take the extra word when the count does not divide
 * evenly, so no column is more than one longer than the next. Empty input
 * yields `columns` empty columns, so the grid keeps its shape while the
 * phrase is still loading.
 */
export function chunkPhraseColumns(
  words: readonly string[],
  columns = 2,
): NumberedWord[][] {
  const cols = Math.max(1, Math.floor(columns));
  const out: NumberedWord[][] = [];
  const base = Math.floor(words.length / cols);
  const extra = words.length % cols;
  let next = 0;
  for (let c = 0; c < cols; c += 1) {
    const take = base + (c < extra ? 1 : 0);
    const column: NumberedWord[] = [];
    for (let i = 0; i < take; i += 1) {
      column.push({ position: next + 1, word: words[next] as string });
      next += 1;
    }
    out.push(column);
  }
  return out;
}

/** The phrase as the Copy button puts it on the clipboard: the words
 *  space-separated on one line, no numbers, so it pastes back into any
 *  phrase field (including this app's own grid input). */
export function phraseForClipboard(words: readonly string[]): string {
  return words.join(" ");
}
