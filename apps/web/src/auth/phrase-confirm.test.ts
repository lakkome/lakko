// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  CONFIRM_WORDS,
  confirmationComplete,
  confirmationErrorMessage,
  confirmationMismatches,
  phraseWords,
  pickConfirmationPositions,
} from "./phrase-confirm";

const PHRASE = Array.from({ length: 24 }, (_, i) => `word${i + 1}`);

/** A deterministic `random` that walks a fixed sequence. */
function sequence(values: number[]): () => number {
  let i = 0;
  return () => {
    const v = values[i % values.length] ?? 0;
    i += 1;
    return v;
  };
}

describe("phraseWords", () => {
  it("splits on any whitespace and drops blanks", () => {
    expect(phraseWords("  a\tb\n c  ")).toEqual(["a", "b", "c"]);
    expect(phraseWords("")).toEqual([]);
  });
});

describe("pickConfirmationPositions", () => {
  it("picks three distinct positions within 1..24, sorted", () => {
    for (let round = 0; round < 200; round += 1) {
      const picked = pickConfirmationPositions(24);
      expect(picked).toHaveLength(CONFIRM_WORDS);
      expect(new Set(picked).size).toBe(CONFIRM_WORDS);
      for (const p of picked) {
        expect(p).toBeGreaterThanOrEqual(1);
        expect(p).toBeLessThanOrEqual(24);
        expect(Number.isInteger(p)).toBe(true);
      }
      expect([...picked].sort((a, b) => a - b)).toEqual(picked);
    }
  });

  it("is deterministic under an injected random source", () => {
    const a = pickConfirmationPositions(24, 3, sequence([0.1, 0.5, 0.9]));
    const b = pickConfirmationPositions(24, 3, sequence([0.1, 0.5, 0.9]));
    expect(a).toEqual(b);
  });

  it("reaches the last position, so no word is never asked for", () => {
    // A random source pinned at 1 must not index past the pool, and must
    // reach position 24.
    const top = pickConfirmationPositions(24, 3, () => 1);
    expect(top).toContain(24);
    expect(top.every((p) => p >= 1 && p <= 24)).toBe(true);
    expect(new Set(top).size).toBe(3);
    // Pinned at 0 takes the first three in order.
    expect(pickConfirmationPositions(24, 3, () => 0)).toEqual([1, 2, 3]);
  });

  it("asks for every word when there are fewer than three, and none of an empty phrase", () => {
    expect(pickConfirmationPositions(2)).toEqual([1, 2]);
    expect(pickConfirmationPositions(0)).toEqual([]);
  });
});

describe("confirmationMismatches", () => {
  it("is empty when every answer matches, ignoring case and surrounding space", () => {
    expect(confirmationMismatches(PHRASE, [3, 10, 24], [" Word3 ", "WORD10", "word24"])).toEqual([]);
  });

  it("names the positions that are wrong, in slot order", () => {
    expect(confirmationMismatches(PHRASE, [3, 10, 24], ["word3", "nope", "word1"])).toEqual([10, 24]);
  });

  it("treats a blank or missing answer as a mismatch", () => {
    expect(confirmationMismatches(PHRASE, [3, 10, 24], ["word3", "", "word24"])).toEqual([10]);
    expect(confirmationMismatches(PHRASE, [3, 10, 24], ["word3"])).toEqual([10, 24]);
  });

  it("never passes a position the phrase does not have", () => {
    expect(confirmationMismatches(PHRASE.slice(0, 5), [7], ["anything"])).toEqual([7]);
  });
});

describe("confirmationComplete", () => {
  it("is true only once every slot has a non-blank answer", () => {
    expect(confirmationComplete([3, 10, 24], ["a", "b", "c"])).toBe(true);
    expect(confirmationComplete([3, 10, 24], ["a", " ", "c"])).toBe(false);
    expect(confirmationComplete([3, 10, 24], ["a", "b"])).toBe(false);
    expect(confirmationComplete([], [])).toBe(false);
  });
});

describe("confirmationErrorMessage", () => {
  it("is one sentence naming the position plus what to do", () => {
    expect(confirmationErrorMessage([7])).toBe(
      "Word 7 does not match. Check your notes and try again.",
    );
    expect(confirmationErrorMessage([7, 19])).toBe(
      "Words 7, 19 do not match. Check your notes and try again.",
    );
    expect(confirmationErrorMessage([])).toBeNull();
  });
});
