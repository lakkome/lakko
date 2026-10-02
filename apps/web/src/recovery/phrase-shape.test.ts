// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Pins the two TypeScript constants of `phrase-shape.ts` against the wasm
 * core, which is the authority (ADR-0064 decision 3: 12 words, 16 bytes).
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { RECOVERY_PHRASE_ENTROPY_BYTES, RECOVERY_PHRASE_WORDS } from "./phrase-shape";

const ABANDON_ABOUT =
  "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

describe("the recovery phrase shape", () => {
  it("matches what the core generates", () => {
    const m = wasm.RecoveryMnemonic.generate();
    expect(m.phrase.split(" ")).toHaveLength(RECOVERY_PHRASE_WORDS);
    expect(m.toEntropy()).toHaveLength(RECOVERY_PHRASE_ENTROPY_BYTES);
  });

  it("accepts the 12-word BIP39 test vector and round-trips its entropy", () => {
    const m = wasm.RecoveryMnemonic.fromPhrase(ABANDON_ABOUT);
    const entropy = m.toEntropy();
    expect(Array.from(entropy)).toEqual(new Array(RECOVERY_PHRASE_ENTROPY_BYTES).fill(0));
    expect(wasm.RecoveryMnemonic.fromEntropy(entropy).phrase).toBe(ABANDON_ABOUT);
  });

  it("refuses the retired 24-word shape", () => {
    const old = `${"abandon ".repeat(23)}art`;
    expect(() => wasm.RecoveryMnemonic.fromPhrase(old)).toThrow();
    expect(() => wasm.RecoveryMnemonic.fromEntropy(new Uint8Array(32))).toThrow();
  });
});
