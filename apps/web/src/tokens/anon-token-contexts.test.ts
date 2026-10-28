// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The web's mirror of the anonymous-token contexts against the REAL core.
 *
 * `AnonTokenContext` is a hand-written union: the wasm surface hands contexts
 * over as plain strings, so nothing in the type system ties the union to
 * `lakko_anon_token::TokenContext::ALL`. A context appended in Rust and
 * missing here would have no pool policy and no copy, and a name spelled
 * differently here would be refused by the core at the first refill. This
 * suite compares the two lists, in wire order, and checks that every context
 * the core names has a pool policy the core can serve.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { ANON_TOKEN_CONTEXTS, MAX_POOL, POOL_TARGET, poolTarget } from "./anon-token-pool";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

describe("the token contexts against the core", () => {
  it("lists the same names as anonTokenContexts(), in wire order", () => {
    expect([...ANON_TOKEN_CONTEXTS]).toEqual(wasm.anonTokenContexts());
  });

  it("is the eleven contexts of wire bytes 1..=11", () => {
    // Literals on purpose: a context appended in Rust fails the comparison
    // above; this one fails when a name is edited on both sides at once.
    expect(wasm.anonTokenContexts()).toEqual([
      "discovery-lookup",
      "discovery-register",
      "invite-consume",
      "ds-knock",
      "push-register",
      "channel-subscribe",
      "directory-search",
      "vault-claim",
      "blob-upload",
      "blob-read",
      "mailbox-write",
      "call-join",
    ]);
  });

  it("has a pool policy for every context, and for nothing else", () => {
    const names = [...wasm.anonTokenContexts()].sort();
    expect(Object.keys(POOL_TARGET).sort()).toEqual(names);
  });

  it("keeps a target the core can blind in one batch and the pool can hold", () => {
    // ADR-0078 section 8: a scheduled refill tops a bucket up to its target
    // in ONE issuance, so the target never exceeds what one batch can carry.
    const maxBatch = wasm.anonTokenMaxBatch();
    for (const context of ANON_TOKEN_CONTEXTS) {
      const target = poolTarget(context);
      expect(target, context).toBeGreaterThanOrEqual(2);
      expect(target, context).toBeLessThanOrEqual(Math.min(maxBatch, MAX_POOL));
    }
  });

  it("the core blinds a batch for every context under its name", () => {
    for (const context of ANON_TOKEN_CONTEXTS) {
      const request = new wasm.AnonTokenRequest(context, 19_675n, poolTarget(context));
      try {
        const body = JSON.parse(request.issueBodyJson()) as {
          context: string;
          blinded_elements: string[];
        };
        expect(body.context).toBe(context);
        expect(body.blinded_elements).toHaveLength(poolTarget(context));
      } finally {
        request.free();
      }
    }
  });

  it("the storage contexts and mailbox-write keep 8 / 4 / 4 / 4 on hand", () => {
    // Chosen, not measured.
    expect(poolTarget("vault-claim")).toBe(8);
    expect(poolTarget("blob-upload")).toBe(4);
    expect(poolTarget("blob-read")).toBe(4);
    expect(poolTarget("mailbox-write")).toBe(4);
  });

  it("the ADR-0078 call-join context keeps 4 on hand", () => {
    // Chosen, not measured: one token per call admission.
    expect(poolTarget("call-join")).toBe(4);
  });
});
