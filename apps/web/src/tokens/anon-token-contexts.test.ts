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
import {
  ANON_TOKEN_CONTEXTS,
  LOW_WATER,
  MAX_POOL,
  REFILL_BATCH,
  lowWater,
  refillBatch,
} from "./anon-token-pool";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

describe("the token contexts against the core", () => {
  it("lists the same names as anonTokenContexts(), in wire order", () => {
    expect([...ANON_TOKEN_CONTEXTS]).toEqual(wasm.anonTokenContexts());
  });

  it("is the ten contexts of wire bytes 1..=10", () => {
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
    ]);
  });

  it("has a pool policy for every context, and for nothing else", () => {
    const names = [...wasm.anonTokenContexts()].sort();
    expect(Object.keys(REFILL_BATCH).sort()).toEqual(names);
    expect(Object.keys(LOW_WATER).sort()).toEqual(names);
  });

  it("asks for a batch the core can blind and the pool can hold", () => {
    const maxBatch = wasm.anonTokenMaxBatch();
    for (const context of ANON_TOKEN_CONTEXTS) {
      const batch = refillBatch(context);
      const low = lowWater(context);
      expect(batch, context).toBeGreaterThanOrEqual(1);
      expect(batch, context).toBeLessThanOrEqual(Math.min(maxBatch, MAX_POOL));
      // A refill must lift the bucket to at least its low-water mark, or
      // every take would start another refill.
      expect(low, context).toBeGreaterThanOrEqual(1);
      expect(low, context).toBeLessThanOrEqual(batch);
    }
  });

  it("the core blinds a batch for every context under its name", () => {
    for (const context of ANON_TOKEN_CONTEXTS) {
      const request = new wasm.AnonTokenRequest(context, 19_675n, refillBatch(context));
      try {
        const body = JSON.parse(request.issueBodyJson()) as {
          context: string;
          blinded_elements: string[];
        };
        expect(body.context).toBe(context);
        expect(body.blinded_elements).toHaveLength(refillBatch(context));
      } finally {
        request.free();
      }
    }
  });

  it("the storage contexts: batch 4 / 2 / 2, refilled once empty", () => {
    // Chosen, not measured (no flow spends them yet).
    expect([refillBatch("vault-claim"), lowWater("vault-claim")]).toEqual([4, 1]);
    expect([refillBatch("blob-upload"), lowWater("blob-upload")]).toEqual([2, 1]);
    expect([refillBatch("blob-read"), lowWater("blob-read")]).toEqual([2, 1]);
  });
});
