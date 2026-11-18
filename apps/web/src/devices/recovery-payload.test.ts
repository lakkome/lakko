// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Unit tests for the ADR-0018 factor (a) recovery payload, v3 since
 * 2026-11-15 (ADR-0074 "Joining after a rotation"): the codec is the core's
 * (`lakko-mnemonic::recovery_payload` through wasm), reached through
 * `recovery-payload.ts`; plus the wasm seal/open round-trip (`sealWithKey`
 * / `openWithKey` — ChaCha20-Poly1305, `nonce(12) ‖ ct+tag`).
 *
 * The DS-opacity pin is the load-bearing test: the bytes that hit
 * the rendezvous mailbox (the tagged row) must never contain the
 * 16-byte recovery-phrase entropy as a plaintext subsequence
 * (invariant #2 — the server only ever sees ciphertext).
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { createHash } from "node:crypto";
import {
  composeRecoveryPlaintext,
  decodeRecoveryPlaintext,
  isRecoveryPayloadRow,
  type RecoveredPredecessor,
} from "./recovery-payload";

const RECOVERY_GRANT_TOKEN_LENGTH = 32;
const RECOVERY_PHRASE_ENTROPY_LENGTH = 16;

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await init({ module_or_path: readFileSync(wasmPath) });
});

function freshGrant(): Uint8Array {
  return crypto.getRandomValues(
    new Uint8Array(RECOVERY_GRANT_TOKEN_LENGTH),
  );
}

function freshSeed(): Uint8Array {
  return crypto.getRandomValues(
    new Uint8Array(RECOVERY_PHRASE_ENTROPY_LENGTH),
  );
}

function freshKey(): Uint8Array {
  return crypto.getRandomValues(new Uint8Array(32));
}

/** `true` when `needle` occurs as a contiguous subsequence of
 *  `haystack`. */
function containsSubsequence(
  haystack: Uint8Array,
  needle: Uint8Array,
): boolean {
  if (needle.length === 0 || haystack.length < needle.length) return false;
  outer: for (let i = 0; i + needle.length <= haystack.length; i += 1) {
    for (let j = 0; j < needle.length; j += 1) {
      if (haystack[i + j] !== needle[j]) continue outer;
    }
    return true;
  }
  return false;
}

const pred = (fill: number, adoptedAtMs: number): RecoveredPredecessor => ({
  seed: new Uint8Array(64).fill(fill),
  adoptedAtMs,
});

/** The core's refusal code of a thrown error (`"<code>: <reason>"`). */
function codeOf(fn: () => unknown): string {
  try {
    fn();
  } catch (err) {
    return err instanceof Error ? (err.message.split(":")[0] ?? "") : "";
  }
  return "no-throw";
}

describe("recovery payload v3 through the core (ADR-0074 'Joining after a rotation')", () => {
  it("pins the golden plaintext vector (contract G-56 section 3.1)", () => {
    const plaintext = composeRecoveryPlaintext(
      wasm,
      new Uint8Array(32).fill(0x01),
      new Uint8Array(16).fill(0x02),
      [pred(0x03, 1_700_000_000_000), pred(0x04, 1_700_000_100_000)],
    );
    expect(plaintext.length).toBe(193);
    expect(createHash("sha256").update(plaintext).digest("hex")).toBe(
      "43260742d3c1e705179d7ac8baf1f0566b5fd9621eb1df08bb9a48e34dd56a41",
    );
  });

  it("round-trips grant, entropy and the earlier keys in order", () => {
    const grant = freshGrant();
    const entropy = freshSeed();
    const preds = [pred(0x31, 1_700_000_000_000), pred(0x32, 1_700_000_000_500), pred(0x33, 2)];
    const plaintext = composeRecoveryPlaintext(wasm, grant, entropy, preds);
    expect(plaintext.length).toBe(49 + 72 * 3);
    const opened = decodeRecoveryPlaintext(wasm, plaintext);
    expect(opened.grant).toEqual(grant);
    expect(opened.phraseEntropy).toEqual(entropy);
    expect(opened.predecessors.map((p) => p.seed[0])).toEqual([0x31, 0x32, 0x33]);
    expect(opened.predecessors.map((p) => p.adoptedAtMs)).toEqual([1_700_000_000_000, 1_700_000_000_500, 2]);
  });

  it("carries no earlier key as count 0 (49 bytes)", () => {
    const plaintext = composeRecoveryPlaintext(wasm, freshGrant(), freshSeed(), []);
    expect(plaintext.length).toBe(49);
    expect(decodeRecoveryPlaintext(wasm, plaintext).predecessors).toHaveLength(0);
  });

  it("refuses what the core refuses, with its frozen codes", () => {
    const grant = freshGrant();
    const entropy = freshSeed();
    // Nine predecessors.
    const nine = Array.from({ length: 9 }, (_, i) => pred(0x40 + i, i));
    expect(codeOf(() => composeRecoveryPlaintext(wasm, grant, entropy, nine))).toBe("too-many-predecessors");
    // A duplicate.
    expect(
      codeOf(() => composeRecoveryPlaintext(wasm, grant, entropy, [pred(0x50, 1), pred(0x50, 2)])),
    ).toBe("duplicate-predecessor");
    // A seed of the current key (the seed the entropy derives).
    const mnemonic = wasm.RecoveryMnemonic.fromEntropy(entropy.slice());
    const current = mnemonic.toSeed();
    mnemonic.free();
    expect(
      codeOf(() => composeRecoveryPlaintext(wasm, grant, entropy, [{ seed: current, adoptedAtMs: 1 }])),
    ).toBe("current-key-predecessor");
    // Wrong lengths on decode: v2's 48-byte payload, and a count that does
    // not match the length.
    expect(codeOf(() => decodeRecoveryPlaintext(wasm, new Uint8Array(48)))).toBe("bad-length");
    const one = composeRecoveryPlaintext(wasm, grant, entropy, [pred(0x60, 1)]);
    expect(codeOf(() => decodeRecoveryPlaintext(wasm, one.slice(0, one.length - 1)))).toBe("bad-length");
    const nineCount = composeRecoveryPlaintext(wasm, grant, entropy, []).slice();
    nineCount[48] = 9;
    expect(codeOf(() => decodeRecoveryPlaintext(wasm, nineCount))).not.toBe("no-throw");
  });
});

describe("seal/open round-trip (wasm sealWithKey/openWithKey)", () => {
  it("round-trips the payload under the same key", () => {
    const key = freshKey();
    const grant = freshGrant();
    const entropy = freshSeed();
    const payload = composeRecoveryPlaintext(wasm, grant, entropy, [pred(0x71, 5)]);
    const sealed = wasm.sealWithKey(key, payload);
    // nonce(12) ‖ ct ‖ tag(16)
    expect(sealed.length).toBe(12 + payload.length + 16);
    const opened = decodeRecoveryPlaintext(wasm, wasm.openWithKey(key, sealed));
    expect(opened.grant).toEqual(grant);
    expect(opened.phraseEntropy).toEqual(entropy);
    expect(opened.predecessors[0]?.seed[0]).toBe(0x71);
  });

  it("fails to open a tampered blob and under the wrong key", () => {
    const key = freshKey();
    const sealed = wasm.sealWithKey(key, composeRecoveryPlaintext(wasm, freshGrant(), freshSeed(), []));
    const tampered = sealed.slice();
    tampered[20] = (tampered[20] ?? 0) ^ 0x01;
    expect(() => wasm.openWithKey(key, tampered)).toThrow();
    expect(() => wasm.openWithKey(freshKey(), sealed)).toThrow();
  });

  it("DS-opacity pin: the published row carries no secret in plaintext", () => {
    const key = freshKey();
    const grant = freshGrant();
    const entropy = freshSeed();
    const seed = crypto.getRandomValues(new Uint8Array(64));
    const sealed = wasm.sealWithKey(
      key,
      composeRecoveryPlaintext(wasm, grant, entropy, [{ seed, adoptedAtMs: 1 }]),
    );
    const row = wasm.wrapRecoveryRow(sealed);
    expect(containsSubsequence(row, entropy)).toBe(false);
    expect(containsSubsequence(row, grant)).toBe(false);
    expect(containsSubsequence(row, seed.slice(0, 16))).toBe(false);
  });
});

describe("v3 carries the recovery phrase as its entropy", () => {
  it("the receiver rebuilds the sender's phrase and seed from 16 bytes", () => {
    const held = wasm.RecoveryMnemonic.generate();
    const phrase = held.phrase;
    const seed = held.toSeed();
    const entropy = held.toEntropy();
    held.free();
    expect(entropy.length).toBe(RECOVERY_PHRASE_ENTROPY_LENGTH);
    const opened = decodeRecoveryPlaintext(
      wasm,
      composeRecoveryPlaintext(wasm, freshGrant(), entropy, []),
    );
    const rebuilt = wasm.RecoveryMnemonic.fromEntropy(opened.phraseEntropy);
    expect(rebuilt.phrase).toBe(phrase);
    expect(rebuilt.toSeed()).toEqual(seed);
    rebuilt.free();
  });
});

describe("mailbox row (the core's framing; the transport's row check)", () => {
  const magic = (v: string): Uint8Array => new TextEncoder().encode(`lakko/recovery-payload/${v}`);
  const tagged = (v: string, sealed: Uint8Array): Uint8Array => {
    const m = magic(v);
    const out = new Uint8Array(m.length + sealed.length);
    out.set(m, 0);
    out.set(sealed, m.length);
    return out;
  };

  it("wraps as v3 and unwraps back", () => {
    const sealed = crypto.getRandomValues(new Uint8Array(124));
    const row = wasm.wrapRecoveryRow(sealed);
    expect(row).toEqual(tagged("v3", sealed));
    expect(isRecoveryPayloadRow(row)).toBe(true);
    expect(wasm.unwrapRecoveryRow(row)).toEqual(sealed);
    expect(wasm.recoveryExporterLabel()).toBe("lakko/device-recovery/v3");
    expect(wasm.recoveryExporterKeyLen()).toBe(32);
    expect(wasm.recoveryPredecessorsMax()).toBe(8);
  });

  it("a v2 row is a recovery row the core refuses (flag day, no dual accept)", () => {
    const row = tagged("v2", crypto.getRandomValues(new Uint8Array(76)));
    // The other polls still skip it...
    expect(isRecoveryPayloadRow(row)).toBe(true);
    // ...and the joiner hands nothing over from it.
    expect(codeOf(() => wasm.unwrapRecoveryRow(row))).toBe("unsupported-row");
  });

  it("a non-recovery row is not misclassified", () => {
    const random = crypto.getRandomValues(new Uint8Array(256));
    random[0] = 0x00;
    expect(isRecoveryPayloadRow(random)).toBe(false);
    expect(wasm.unwrapRecoveryRow(random)).toBeUndefined();
  });
});

describe("exporter-key derivation sanity (Path B)", () => {
  it("distinct keys produce mutually unopenable blobs (cross-pairing isolation)", () => {
    const keyA = freshKey();
    const keyB = freshKey();
    const sealed = wasm.sealWithKey(keyA, composeRecoveryPlaintext(wasm, freshGrant(), freshSeed(), []));
    expect(() => wasm.openWithKey(keyB, sealed)).toThrow();
    expect(wasm.openWithKey(keyA, sealed).length).toBe(49);
  });
});
