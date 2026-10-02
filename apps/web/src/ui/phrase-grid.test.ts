// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { RECOVERY_PHRASE_WORDS } from "../recovery/phrase-shape";
import {
  PHRASE_GRID_NARROW,
  PHRASE_GRID_WIDE,
  PHRASE_GRID_WIDE_MIN_PX,
  chunkPhraseRows,
  numberPhraseWords,
  phraseForClipboard,
  phraseGridShape,
} from "./phrase-grid";

const WORDS = Array.from({ length: RECOVERY_PHRASE_WORDS }, (_, i) => `w${i + 1}`);

describe("the phrase grid shape", () => {
  it("is 3 x 4 at phone width and 4 x 3 on a wide screen", () => {
    expect(PHRASE_GRID_NARROW).toEqual({ columns: 3, rows: 4 });
    expect(PHRASE_GRID_WIDE).toEqual({ columns: 4, rows: 3 });
    expect(phraseGridShape(375)).toEqual(PHRASE_GRID_NARROW);
    expect(phraseGridShape(PHRASE_GRID_WIDE_MIN_PX - 1)).toEqual(PHRASE_GRID_NARROW);
    expect(phraseGridShape(PHRASE_GRID_WIDE_MIN_PX)).toEqual(PHRASE_GRID_WIDE);
    expect(phraseGridShape(1440)).toEqual(PHRASE_GRID_WIDE);
  });

  it("holds exactly the whole phrase in both shapes", () => {
    for (const shape of [PHRASE_GRID_NARROW, PHRASE_GRID_WIDE]) {
      expect(shape.columns * shape.rows).toBe(RECOVERY_PHRASE_WORDS);
    }
  });

  it("uses the breakpoint the stylesheet uses", () => {
    const css = readFileSync(fileURLToPath(new URL("../styles.css", import.meta.url)), "utf8");
    expect(css).toContain(
      `@media (min-width: ${PHRASE_GRID_WIDE_MIN_PX}px) {\n  .lk-phrase-cols,\n  .lk-phrase-grid {`,
    );
  });
});

describe("numberPhraseWords", () => {
  it("numbers the words 1..12 in phrase order", () => {
    const numbered = numberPhraseWords(WORDS);
    expect(numbered.map((n) => n.position)).toEqual(WORDS.map((_, i) => i + 1));
    expect(numbered.map((n) => n.word)).toEqual(WORDS);
  });
});

describe("chunkPhraseRows", () => {
  it("reads across: 1 2 3 / 4 5 6 / 7 8 9 / 10 11 12 at phone width", () => {
    const rows = chunkPhraseRows(WORDS, PHRASE_GRID_NARROW.columns);
    expect(rows.map((r) => r.map((n) => n.position))).toEqual([
      [1, 2, 3],
      [4, 5, 6],
      [7, 8, 9],
      [10, 11, 12],
    ]);
    expect(rows).toHaveLength(PHRASE_GRID_NARROW.rows);
  });

  it("reads across: 1 2 3 4 / 5 6 7 8 / 9 10 11 12 on a wide screen", () => {
    const rows = chunkPhraseRows(WORDS, PHRASE_GRID_WIDE.columns);
    expect(rows.map((r) => r.map((n) => n.position))).toEqual([
      [1, 2, 3, 4],
      [5, 6, 7, 8],
      [9, 10, 11, 12],
    ]);
    expect(rows).toHaveLength(PHRASE_GRID_WIDE.rows);
  });

  it("keeps every word exactly once, in order, whatever the column count", () => {
    for (const columns of [1, 2, 3, 4, 5]) {
      expect(chunkPhraseRows(WORDS, columns).flat().map((n) => n.word)).toEqual(WORDS);
    }
  });

  it("leaves the last row short when the count does not divide", () => {
    expect(chunkPhraseRows(WORDS.slice(0, 11), 4).map((r) => r.length)).toEqual([4, 4, 3]);
  });

  it("yields no rows for an empty phrase and never fewer than one column", () => {
    expect(chunkPhraseRows([], 3)).toEqual([]);
    expect(chunkPhraseRows(WORDS, 0)).toHaveLength(RECOVERY_PHRASE_WORDS);
  });
});

describe("phraseForClipboard", () => {
  it("is the words on one line, space-separated, without numbers", () => {
    expect(phraseForClipboard(["a", "b", "c"])).toBe("a b c");
  });
});
