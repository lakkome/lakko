// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Unit tests for the ADR-0018 factor (a) recovery-payload wire
 * module (`recovery-payload.ts`) plus the wasm seal/open round-trip
 * (`sealWithKey` / `openWithKey` — ChaCha20-Poly1305,
 * `nonce(12) ‖ ct+tag`).
 *
 * The DS-opacity pin is the load-bearing test: the bytes that hit
 * the rendezvous mailbox (the tagged row) must never contain the
 * 64-byte AccountIdentity seed as a plaintext subsequence
 * (invariant #2 — the server only ever sees ciphertext).
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  RECOVERY_GRANT_TOKEN_LENGTH,
  RECOVERY_PAYLOAD_LENGTH,
  RECOVERY_PHRASE_SEED_LENGTH,
  buildRecoveryPayload,
  isRecoveryPayloadRow,
  parseRecoveryPayload,
  unwrapRecoveryRow,
  wrapRecoveryRow,
} from "./recovery-payload";

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
    new Uint8Array(RECOVERY_PHRASE_SEED_LENGTH),
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

describe("recovery payload build/parse (ADR-0018 factor a)", () => {
  it("round-trips grant ‖ seed through build + parse", () => {
    const grant = freshGrant();
    const seed = freshSeed();
    const payload = buildRecoveryPayload(grant, seed);
    expect(payload.length).toBe(RECOVERY_PAYLOAD_LENGTH);
    const parsed = parseRecoveryPayload(payload);
    expect(parsed.grantToken).toEqual(grant);
    expect(parsed.phraseSeed).toEqual(seed);
  });

  it("rejects a wrong-length grant token", () => {
    expect(() =>
      buildRecoveryPayload(new Uint8Array(16), freshSeed()),
    ).toThrow();
  });

  it("rejects a wrong-length phrase seed", () => {
    expect(() =>
      buildRecoveryPayload(freshGrant(), new Uint8Array(32)),
    ).toThrow();
  });

  it("rejects a wrong-length payload on parse", () => {
    expect(() => parseRecoveryPayload(new Uint8Array(95))).toThrow();
    expect(() => parseRecoveryPayload(new Uint8Array(97))).toThrow();
    expect(() => parseRecoveryPayload(new Uint8Array(0))).toThrow();
  });
});

describe("seal/open round-trip (wasm sealWithKey/openWithKey)", () => {
  it("round-trips the payload under the same key", () => {
    const key = freshKey();
    const grant = freshGrant();
    const seed = freshSeed();
    const payload = buildRecoveryPayload(grant, seed);
    const sealed = wasm.sealWithKey(key, payload);
    // nonce(12) ‖ ct(96) ‖ tag(16)
    expect(sealed.length).toBe(12 + RECOVERY_PAYLOAD_LENGTH + 16);
    const opened = wasm.openWithKey(key, sealed);
    expect(opened).toEqual(payload);
    const parsed = parseRecoveryPayload(opened);
    expect(parsed.grantToken).toEqual(grant);
    expect(parsed.phraseSeed).toEqual(seed);
  });

  it("fails to open a tampered blob", () => {
    const key = freshKey();
    const sealed = wasm.sealWithKey(
      key,
      buildRecoveryPayload(freshGrant(), freshSeed()),
    );
    // Flip one ciphertext byte (past the 12-byte nonce).
    const tampered = sealed.slice();
    tampered[20] = (tampered[20] ?? 0) ^ 0x01;
    expect(() => wasm.openWithKey(key, tampered)).toThrow();
  });

  it("fails to open under the wrong key", () => {
    const sealed = wasm.sealWithKey(
      freshKey(),
      buildRecoveryPayload(freshGrant(), freshSeed()),
    );
    expect(() => wasm.openWithKey(freshKey(), sealed)).toThrow();
  });

  it("DS-opacity pin: the published row never contains the seed", () => {
    const key = freshKey();
    const grant = freshGrant();
    const seed = freshSeed();
    const sealed = wasm.sealWithKey(key, buildRecoveryPayload(grant, seed));
    const row = wrapRecoveryRow(sealed);
    // Neither the sealed blob nor the tagged row that actually hits
    // the DS may carry the 64-byte seed (or the grant) in plaintext.
    expect(containsSubsequence(sealed, seed)).toBe(false);
    expect(containsSubsequence(row, seed)).toBe(false);
    expect(containsSubsequence(row, grant)).toBe(false);
  });
});

describe("mailbox row wrap/unwrap", () => {
  it("wrap + unwrap round-trips the sealed blob", () => {
    const sealed = crypto.getRandomValues(new Uint8Array(124));
    const row = wrapRecoveryRow(sealed);
    expect(isRecoveryPayloadRow(row)).toBe(true);
    expect(unwrapRecoveryRow(row)).toEqual(sealed);
  });

  it("a non-recovery row is not misclassified", () => {
    // A history-bundle-ish / Welcome-ish random row.
    const random = crypto.getRandomValues(new Uint8Array(256));
    // Force the first byte away from the magic's first byte so the
    // (already astronomically unlikely) random collision is zero.
    random[0] = 0x00;
    expect(isRecoveryPayloadRow(random)).toBe(false);
    expect(unwrapRecoveryRow(random)).toBeNull();
  });

  it("a bare magic prefix with no blob is not a valid row", () => {
    const magicOnly = wrapRecoveryRow(new Uint8Array(0));
    expect(isRecoveryPayloadRow(magicOnly)).toBe(false);
    expect(unwrapRecoveryRow(magicOnly)).toBeNull();
  });
});

describe("exporter-key derivation sanity (Path B)", () => {
  it("distinct keys produce mutually unopenable blobs (cross-pairing isolation)", () => {
    // The exporter context is the per-pairing rendezvous pseudonym,
    // so two pairings derive different keys. Model the consequence:
    // a blob sealed for pairing A must not open under pairing B's key.
    const keyA = freshKey();
    const keyB = freshKey();
    const sealed = wasm.sealWithKey(
      keyA,
      buildRecoveryPayload(freshGrant(), freshSeed()),
    );
    expect(() => wasm.openWithKey(keyB, sealed)).toThrow();
    expect(wasm.openWithKey(keyA, sealed).length).toBe(
      RECOVERY_PAYLOAD_LENGTH,
    );
  });
});
