// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §3 — username verification state machine: verified, mismatch,
 * changed claim, single-flight, retry of a failed lookup, and a claim that
 * fails the rule never reaching discovery.
 *
 * The rule is the REAL core's `validateUsername`; discovery is a stand-in that
 * answers what a real lookup + identity fetch would.
 *
 * ## Discrimination (verified by running the broken variant)
 *
 * Without the in-flight map, "concurrent requests for one claim share one
 * lookup" fails with `expected "vi.fn()" to be called 1 times, but got 3 times`.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import type { StoredVerification } from "./profile-store";
import {
  ERROR_RETRY_MS,
  MISMATCH_RECHECK_MS,
  UsernameVerifier,
  verdictFor,
  type UsernameResolution,
} from "./username-verification";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const ALICE_KEY = new Uint8Array(32).fill(0xa1);
const MALLORY_KEY = new Uint8Array(32).fill(0x3c);

function harness(directory: Record<string, Uint8Array | "throw">) {
  let now = 5_000_000;
  const cache = new Map<string, StoredVerification>();
  const resolve = vi.fn(async (username: string): Promise<UsernameResolution> => {
    const hit = directory[username];
    if (hit === "throw") throw new Error("network");
    return hit === undefined ? { kind: "unregistered" } : { kind: "key", signatureKey: hit };
  });
  const verifier = new UsernameVerifier({
    resolve,
    validate: (u) => wasm.validateUsername(u),
    cached: (k, u) => cache.get(`${k}|${u}`),
    record: (k, u, v) => {
      cache.set(`${k}|${u}`, v);
    },
    now: () => now,
    decodeKey: base64Decode,
  });
  return {
    verifier,
    resolve,
    cache,
    advance: (ms: number) => {
      now += ms;
    },
  };
}

describe("verdictFor", () => {
  it("verified only when the resolved key is the sender's", () => {
    expect(verdictFor(ALICE_KEY, { kind: "key", signatureKey: ALICE_KEY })).toBe("verified");
    expect(verdictFor(ALICE_KEY, { kind: "key", signatureKey: MALLORY_KEY })).toBe("mismatch");
    expect(verdictFor(ALICE_KEY, { kind: "unregistered" })).toBe("mismatch");
  });
});

describe("UsernameVerifier", () => {
  it("verifies a claim that resolves to the sender's own key", async () => {
    const h = harness({ alice: ALICE_KEY });
    expect(await h.verifier.ensure(base64Encode(ALICE_KEY), "alice")).toBe("verified");
    expect(h.verifier.state(base64Encode(ALICE_KEY), "alice")).toBe("verified");
  });

  it("refuses a claim that resolves to someone else", async () => {
    const h = harness({ alice: ALICE_KEY });
    const mallory = base64Encode(MALLORY_KEY);
    expect(await h.verifier.ensure(mallory, "alice")).toBe("mismatch");
    expect(h.verifier.state(mallory, "alice")).toBe("mismatch");
  });

  it("refuses an unregistered claim", async () => {
    const h = harness({});
    expect(await h.verifier.ensure(base64Encode(ALICE_KEY), "alice")).toBe("mismatch");
  });

  it("a changed claim re-verifies; the old verdict says nothing about it", async () => {
    const h = harness({ alice: ALICE_KEY, bob: MALLORY_KEY });
    const k = base64Encode(ALICE_KEY);
    expect(await h.verifier.ensure(k, "alice")).toBe("verified");
    expect(h.verifier.state(k, "bob")).toBeUndefined();
    expect(await h.verifier.ensure(k, "bob")).toBe("mismatch");
    expect(h.resolve).toHaveBeenCalledTimes(2);
  });

  it("a verified claim is cached: no second lookup", async () => {
    const h = harness({ alice: ALICE_KEY });
    const k = base64Encode(ALICE_KEY);
    await h.verifier.ensure(k, "alice");
    h.advance(MISMATCH_RECHECK_MS * 10);
    await h.verifier.ensure(k, "alice");
    expect(h.resolve).toHaveBeenCalledTimes(1);
  });

  it("concurrent requests for one claim share one lookup", async () => {
    const h = harness({ alice: ALICE_KEY });
    const k = base64Encode(ALICE_KEY);
    const results = await Promise.all([
      h.verifier.ensure(k, "alice"),
      h.verifier.ensure(k, "alice"),
      h.verifier.ensure(k, "alice"),
    ]);
    expect(results).toEqual(["verified", "verified", "verified"]);
    expect(h.resolve).toHaveBeenCalledTimes(1);
  });

  it("shows pending while the lookup runs", async () => {
    const h = harness({ alice: ALICE_KEY });
    const k = base64Encode(ALICE_KEY);
    const done = h.verifier.ensure(k, "alice");
    expect(h.verifier.state(k, "alice")).toBe("pending");
    await done;
    expect(h.verifier.state(k, "alice")).toBe("verified");
  });

  it("a failed lookup is an error, retried only after the retry interval", async () => {
    const h = harness({ alice: "throw" });
    const k = base64Encode(ALICE_KEY);
    expect(await h.verifier.ensure(k, "alice")).toBe("error");
    await h.verifier.ensure(k, "alice");
    expect(h.resolve).toHaveBeenCalledTimes(1);
    h.advance(ERROR_RETRY_MS);
    await h.verifier.ensure(k, "alice");
    expect(h.resolve).toHaveBeenCalledTimes(2);
  });

  it("a mismatch is re-checked only after a long interval", async () => {
    const h = harness({});
    const k = base64Encode(ALICE_KEY);
    await h.verifier.ensure(k, "alice");
    h.advance(MISMATCH_RECHECK_MS - 1);
    await h.verifier.ensure(k, "alice");
    expect(h.resolve).toHaveBeenCalledTimes(1);
    h.advance(1);
    await h.verifier.ensure(k, "alice");
    expect(h.resolve).toHaveBeenCalledTimes(2);
  });

  it("a claim that fails the username rule is never looked up", async () => {
    const h = harness({ "al-ice": ALICE_KEY });
    const k = base64Encode(ALICE_KEY);
    expect(await h.verifier.ensure(k, "al-ice")).toBe("mismatch");
    expect(await h.verifier.ensure(k, "Alice")).toBe("mismatch");
    expect(h.resolve).not.toHaveBeenCalled();
  });

  it("learn records a binding a lookup already proved", async () => {
    const h = harness({});
    const k = base64Encode(ALICE_KEY);
    h.verifier.learn(k, "alice");
    expect(await h.verifier.ensure(k, "alice")).toBe("verified");
    expect(h.resolve).not.toHaveBeenCalled();
  });
});
