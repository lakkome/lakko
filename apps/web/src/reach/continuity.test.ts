// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0073 R.4 - the continuity key's window (`continuity.ts`), and that the
 *  wasm core derives one key from public values. ADR-0078: the resetting
 *  device no longer registers it (no peer can address its mailbox), so there
 *  is no marker any more. */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { CONTINUITY_KEY_TTL_MS, continuityExpiryMs, continuityKeyUsable } from "./continuity";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

describe("the continuity window (D2: 30 days from the reset's signed_at)", () => {
  it("is the DS journal retention", () => {
    expect(CONTINUITY_KEY_TTL_MS).toBe(30 * 24 * 60 * 60 * 1000);
    expect(continuityExpiryMs(1000)).toBe(1000 + CONTINUITY_KEY_TTL_MS);
  });
  it("a peer uses it inside the same window only", () => {
    expect(continuityKeyUsable(1000, 1000 + CONTINUITY_KEY_TTL_MS - 1)).toBe(true);
    expect(continuityKeyUsable(1000, 1000 + CONTINUITY_KEY_TTL_MS)).toBe(false);
  });
});

describe("both sides derive one key (the wasm core)", () => {
  const ACCOUNT = new Uint8Array(16).fill(5);
  it("the seed is a function of the account id and the generation only", () => {
    const a = wasm.continuityReachSeed(ACCOUNT, 2n);
    const b = wasm.continuityReachSeed(ACCOUNT, 2n);
    expect(Array.from(a)).toEqual(Array.from(b));
    expect(a.length).toBe(32);
    expect(Array.from(wasm.continuityReachSeed(ACCOUNT, 3n))).not.toEqual(Array.from(a));
    expect(Array.from(wasm.continuityReachSeed(new Uint8Array(16).fill(6), 2n))).not.toEqual(Array.from(a));
  });
  it("the reach verifying key is the one a signer over the seed proves under", () => {
    const seed = wasm.continuityReachSeed(ACCOUNT, 2n);
    const registered = wasm.reachVerifyingKey(seed);
    const signer = wasm.DsProofSigner.forReach(wasm.continuityReachSeed(ACCOUNT, 2n));
    try {
      expect(Array.from(signer.verifyingKey())).toEqual(Array.from(registered));
    } finally {
      signer.free();
    }
  });
});
