// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The contract the three-words form relies on from the core
 * (`phraseCheckRandomPositions`, `phraseCheckWords`), pinned through the
 * wasm boundary: positions are three distinct word numbers in 1..=12 in the
 * order drawn, `words[i]` answers `positions[i]`, typed words are trimmed
 * and lower-cased, a wrong or unknown word is `false`, and malformed input
 * throws.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../../wasm/lakko_ffi_wasm";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

describe("the local three-word check", () => {
  it("draws three distinct positions in 1..=12, and not always sorted", () => {
    let unsorted = 0;
    for (let i = 0; i < 200; i += 1) {
      const p = Array.from(wasm.phraseCheckRandomPositions());
      expect(p).toHaveLength(3);
      expect(new Set(p).size).toBe(3);
      for (const n of p) {
        expect(n).toBeGreaterThanOrEqual(1);
        expect(n).toBeLessThanOrEqual(12);
      }
      if (!(p[0]! < p[1]! && p[1]! < p[2]!)) unsorted += 1;
    }
    // The form shows them in the order drawn; a sorted draw every time
    // would mean the order carries nothing.
    expect(unsorted).toBeGreaterThan(0);
  });

  it("matches the phrase's words at the asked positions, in the asked order", () => {
    const m = wasm.RecoveryMnemonic.generate();
    const all = m.phrase.split(" ");
    const positions = Uint8Array.from([7, 3, 12]);
    const right = [all[6]!, all[2]!, all[11]!];
    expect(wasm.phraseCheckWords(m, positions, right)).toBe(true);
    // Trimmed and lower-cased.
    expect(wasm.phraseCheckWords(m, positions, right.map((w) => `  ${w.toUpperCase()} `))).toBe(true);
    // The right words in phrase order are the wrong answer to "7, 3, 12"
    // (unless two of them happen to be the same word).
    const sorted = [all[2]!, all[6]!, all[11]!];
    if (new Set(sorted).size === 3) {
      expect(wasm.phraseCheckWords(m, positions, sorted)).toBe(false);
    }
    m.free();
  });

  it("answers false for a wrong or unknown word, without throwing", () => {
    const m = wasm.RecoveryMnemonic.generate();
    const all = m.phrase.split(" ");
    const positions = Uint8Array.from([1, 2, 3]);
    expect(wasm.phraseCheckWords(m, positions, [all[0]!, all[1]!, "notaword"])).toBe(false);
    expect(wasm.phraseCheckWords(m, positions, ["", "", ""])).toBe(false);
    m.free();
  });

  it("throws on malformed input", () => {
    const m = wasm.RecoveryMnemonic.generate();
    const w = ["a", "b", "c"];
    expect(() => wasm.phraseCheckWords(m, Uint8Array.from([1, 1, 2]), w)).toThrow(/repeated-position/);
    expect(() => wasm.phraseCheckWords(m, Uint8Array.from([1, 2, 13]), w)).toThrow(/position-out-of-range/);
    expect(() => wasm.phraseCheckWords(m, Uint8Array.from([1, 2]), w)).toThrow(/wrong-position-count/);
    expect(() => wasm.phraseCheckWords(m, Uint8Array.from([1, 2, 3]), ["a", "b"])).toThrow(/wrong-word-count/);
    m.free();
  });
});
