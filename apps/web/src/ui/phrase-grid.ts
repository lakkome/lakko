// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Layout rule for the recovery-phrase grid (ADR-0072; the maintainer's
 * requirement that the 12 words fit one screen): the words read ACROSS the
 * rows, row-major (1 2 3 / 4 5 6 / ...), in a grid that is 3 columns x 4
 * rows at phone width and 4 columns x 3 rows on a wide screen. The same
 * order is used by the read-only grid at signup (`PhraseGrid.tsx`), the
 * check step and the entry grid (`PhraseInput.tsx`), so a person copies and
 * types the words in one and the same reading order.
 *
 * The rule is here, apart from the components, so it can be tested without
 * the Solid transform. The components render the words as one flat list in
 * position order and CSS sets the column count (`--lk-phrase-columns` is 3,
 * and 4 from `PHRASE_GRID_WIDE_MIN_PX` up), which is what makes DOM order,
 * tab order and reading order agree.
 */

/** One numbered word in the grid. `position` is 1-based. */
export interface NumberedWord {
  readonly position: number;
  readonly word: string;
}

/** Columns and rows of the grid. */
export interface PhraseGridShape {
  readonly columns: number;
  readonly rows: number;
}

/** Phone width: 3 columns x 4 rows. */
export const PHRASE_GRID_NARROW: PhraseGridShape = { columns: 3, rows: 4 };

/** Wide screens: 4 columns x 3 rows. */
export const PHRASE_GRID_WIDE: PhraseGridShape = { columns: 4, rows: 3 };

/** The viewport width from which the wide shape applies. `styles.css` uses
 *  the same number in its media query; `phrase-grid.test.ts` pins the two
 *  together. */
export const PHRASE_GRID_WIDE_MIN_PX = 720;

/** The shape for a viewport `widthPx` wide. */
export function phraseGridShape(widthPx: number): PhraseGridShape {
  return widthPx >= PHRASE_GRID_WIDE_MIN_PX ? PHRASE_GRID_WIDE : PHRASE_GRID_NARROW;
}

/** The words with their 1-based positions, in phrase order: the order they
 *  are rendered, read and tabbed through. */
export function numberPhraseWords(words: readonly string[]): NumberedWord[] {
  return words.map((word, i) => ({ position: i + 1, word }));
}

/**
 * The grid as rows, row-major: row `r` holds positions `r * columns + 1`
 * through `(r + 1) * columns`. The last row is shorter when the count does
 * not divide evenly. This is what the CSS grid produces from the flat list;
 * it exists so the rule can be asserted.
 */
export function chunkPhraseRows(
  words: readonly string[],
  columns: number = PHRASE_GRID_NARROW.columns,
): NumberedWord[][] {
  const cols = Math.max(1, Math.floor(columns));
  const numbered = numberPhraseWords(words);
  const out: NumberedWord[][] = [];
  for (let i = 0; i < numbered.length; i += cols) {
    out.push(numbered.slice(i, i + cols));
  }
  return out;
}

/** The phrase as the Copy button puts it on the clipboard: the words
 *  space-separated on one line, no numbers, so it pastes back into any
 *  phrase field (including this app's own grid input). */
export function phraseForClipboard(words: readonly string[]): string {
  return words.join(" ");
}
