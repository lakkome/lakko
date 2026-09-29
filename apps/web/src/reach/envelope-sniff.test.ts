// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0058 — the foreground sniff that keeps the scoped-discovery envelopes
 * out of the chat parser, over the REAL core (composers and dispatcher).
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { sniffScopedEnvelope } from "./envelope-sniff";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

describe("the scoped-discovery sniff", () => {
  it("names the visibility and reach-state envelopes", () => {
    expect(sniffScopedEnvelope(wasm, wasm.composeVisibilityEnvelope(wasm.defaultVisibilitySetting()))).toBe(
      "visibility",
    );
    expect(sniffScopedEnvelope(wasm, wasm.composeReachStateEnvelope(new wasm.ReachState([], 3n)))).toBe(
      "reach-state",
    );
  });

  it("names a committer-delegation envelope", () => {
    const added = wasm.reachVerifyingKey(new Uint8Array(32).fill(9));
    const mailbox = wasm.IdentityPseudonym.fromIdentity(added).asBytes();
    const blob = wasm.signDelegationBlob(
      new Uint8Array(32).fill(4),
      mailbox,
      wasm.reachVerifyingKey(new Uint8Array(32).fill(1)),
      BigInt(Date.now() + 60_000),
      new Uint8Array(16),
    );
    const env = wasm.composeCommitterDelegationEnvelope(new wasm.CommitterDelegationEnvelope(added, [blob]));
    expect(sniffScopedEnvelope(wasm, env)).toBe("committer-delegation");
  });

  it("leaves chat text, other service envelopes and a leading `l` alone", () => {
    expect(sniffScopedEnvelope(wasm, new TextEncoder().encode('{"v":1,"text":"hi"}'))).toBeNull();
    expect(sniffScopedEnvelope(wasm, new TextEncoder().encode("lunch at six?"))).toBeNull();
    expect(sniffScopedEnvelope(wasm, wasm.composePinEnvelope(1n, true, 1n))).toBeNull();
    expect(sniffScopedEnvelope(wasm, new Uint8Array(0))).toBeNull();
  });
});
