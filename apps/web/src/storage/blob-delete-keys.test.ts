// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 section 6 with ADR-0074 — a blob's delete key is the same before
 * and after an account key rotation, against the real core.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { blobDeleteVaultKeys } from "./blob-delete-keys";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const seed = (n: number): Uint8Array => new Uint8Array(64).fill(n);
const blobId = new Uint8Array(32).fill(7);

describe("the blob delete key across a rotation", () => {
  it("is the same before and after", () => {
    // Before: no predecessor, the lineage seed IS the account seed.
    const before = wasm.Identity.fromSeed(seed(1));
    const k1 = blobDeleteVaultKeys(wasm, seed(1), before).blobDeletePublicKey(blobId);
    // After: a new account identity; the lineage seed is the predecessor's.
    const after = wasm.Identity.fromSeed(seed(2));
    const k2 = blobDeleteVaultKeys(wasm, seed(1), after).blobDeletePublicKey(blobId);
    expect(k2).toEqual(k1);
    // The current identity alone (what was used before) would have moved.
    const moved = blobDeleteVaultKeys(wasm, null, after).blobDeletePublicKey(blobId);
    expect(moved).not.toEqual(k1);
    before.free();
    after.free();
  });

  it("falls back to the current identity when no seed is held", () => {
    const current = wasm.Identity.fromSeed(seed(3));
    const viaCurrent = blobDeleteVaultKeys(wasm, null, current).blobDeletePublicKey(blobId);
    const viaSeed = blobDeleteVaultKeys(wasm, seed(3), current).blobDeletePublicKey(blobId);
    expect(viaCurrent).toEqual(viaSeed);
    current.free();
  });
});
