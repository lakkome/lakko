// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { chunkPhraseColumns, phraseForClipboard } from "./phrase-grid";

const WORDS = Array.from({ length: 24 }, (_, i) => `w${i + 1}`);

describe("chunkPhraseColumns", () => {
  it("lays 24 words out as 1..12 down the left and 13..24 down the right", () => {
    const [left, right] = chunkPhraseColumns(WORDS);
    expect(left?.map((n) => n.position)).toEqual(Array.from({ length: 12 }, (_, i) => i + 1));
    expect(right?.map((n) => n.position)).toEqual(Array.from({ length: 12 }, (_, i) => i + 13));
    expect(left?.[0]).toEqual({ position: 1, word: "w1" });
    expect(right?.[11]).toEqual({ position: 24, word: "w24" });
  });

  it("keeps every word exactly once, in order", () => {
    const flat = chunkPhraseColumns(WORDS, 3).flat();
    expect(flat.map((n) => n.word)).toEqual(WORDS);
    expect(flat.map((n) => n.position)).toEqual(WORDS.map((_, i) => i + 1));
  });

  it("gives the extra words to the first columns when the count does not divide", () => {
    const cols = chunkPhraseColumns(WORDS.slice(0, 23));
    expect(cols.map((c) => c.length)).toEqual([12, 11]);
    const three = chunkPhraseColumns(WORDS.slice(0, 7), 3);
    expect(three.map((c) => c.length)).toEqual([3, 2, 2]);
  });

  it("keeps the grid shape for an empty phrase", () => {
    expect(chunkPhraseColumns([])).toEqual([[], []]);
  });

  it("never produces fewer than one column", () => {
    expect(chunkPhraseColumns(WORDS, 0)).toHaveLength(1);
  });
});

describe("phraseForClipboard", () => {
  it("is the words on one line, space-separated, without numbers", () => {
    expect(phraseForClipboard(["a", "b", "c"])).toBe("a b c");
  });
});
