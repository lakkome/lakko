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
 * 16-byte recovery-phrase entropy as a plaintext subsequence
 * (invariant #2 — the server only ever sees ciphertext).
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  RECOVERY_EXPORTER_LABEL,
  RECOVERY_GRANT_TOKEN_LENGTH,
  RECOVERY_PAYLOAD_LENGTH,
  RECOVERY_PHRASE_ENTROPY_LENGTH,
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

describe("recovery payload build/parse (ADR-0018 factor a)", () => {
  it("round-trips grant ‖ entropy through build + parse", () => {
    const grant = freshGrant();
    const seed = freshSeed();
    const payload = buildRecoveryPayload(grant, seed);
    expect(payload.length).toBe(RECOVERY_PAYLOAD_LENGTH);
    const parsed = parseRecoveryPayload(payload);
    expect(parsed.grantToken).toEqual(grant);
    expect(parsed.phraseEntropy).toEqual(seed);
  });

  it("rejects a wrong-length grant token", () => {
    expect(() =>
      buildRecoveryPayload(new Uint8Array(16), freshSeed()),
    ).toThrow();
  });

  it("rejects a wrong-length phrase entropy", () => {
    expect(() =>
      buildRecoveryPayload(freshGrant(), new Uint8Array(64)),
    ).toThrow();
  });

  it("rejects a wrong-length payload on parse", () => {
    expect(() => parseRecoveryPayload(new Uint8Array(47))).toThrow();
    expect(() => parseRecoveryPayload(new Uint8Array(49))).toThrow();
    expect(() => parseRecoveryPayload(new Uint8Array(0))).toThrow();
    // The v1 plaintext (grant(32) ‖ seed(64)) is refused.
    expect(() => parseRecoveryPayload(new Uint8Array(96))).toThrow();
  });
});

describe("seal/open round-trip (wasm sealWithKey/openWithKey)", () => {
  it("round-trips the payload under the same key", () => {
    const key = freshKey();
    const grant = freshGrant();
    const seed = freshSeed();
    const payload = buildRecoveryPayload(grant, seed);
    const sealed = wasm.sealWithKey(key, payload);
    // nonce(12) ‖ ct(48) ‖ tag(16)
    expect(sealed.length).toBe(12 + RECOVERY_PAYLOAD_LENGTH + 16);
    const opened = wasm.openWithKey(key, sealed);
    expect(opened).toEqual(payload);
    const parsed = parseRecoveryPayload(opened);
    expect(parsed.grantToken).toEqual(grant);
    expect(parsed.phraseEntropy).toEqual(seed);
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
    // the DS may carry the 16-byte entropy (or the grant) in plaintext.
    expect(containsSubsequence(sealed, seed)).toBe(false);
    expect(containsSubsequence(row, seed)).toBe(false);
    expect(containsSubsequence(row, grant)).toBe(false);
  });
});

describe("v2 carries the recovery phrase as its entropy", () => {
  it("the receiver rebuilds the sender's phrase and seed from 16 bytes", () => {
    // Sender: the phrase it holds -> entropy.
    const held = wasm.RecoveryMnemonic.generate();
    const phrase = held.phrase;
    const seed = held.toSeed();
    const entropy = held.toEntropy();
    held.free();
    expect(entropy.length).toBe(RECOVERY_PHRASE_ENTROPY_LENGTH);
    expect(phrase.split(" ")).toHaveLength(12);

    const key = freshKey();
    const grant = freshGrant();
    const sealed = wasm.sealWithKey(key, buildRecoveryPayload(grant, entropy));

    // Receiver: open -> split -> phrase and 64-byte seed.
    const parsed = parseRecoveryPayload(wasm.openWithKey(key, sealed));
    expect(parsed.grantToken).toEqual(grant);
    const rebuilt = wasm.RecoveryMnemonic.fromEntropy(parsed.phraseEntropy);
    expect(rebuilt.phrase).toBe(phrase);
    expect(rebuilt.toSeed()).toEqual(seed);
    expect(rebuilt.toSeed().length).toBe(64);
    rebuilt.free();
  });

  it("entropy of another length is not a phrase", () => {
    expect(() => wasm.RecoveryMnemonic.fromEntropy(new Uint8Array(32))).toThrow();
    expect(() => wasm.RecoveryMnemonic.fromEntropy(new Uint8Array(15))).toThrow();
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

  it("the wire is v2: a v1-tagged row is not a recovery row", () => {
    const sealed = crypto.getRandomValues(new Uint8Array(76));
    const magic = (v: string): Uint8Array => new TextEncoder().encode(`lakko/recovery-payload/${v}`);
    const tagged = (v: string): Uint8Array => {
      const m = magic(v);
      const out = new Uint8Array(m.length + sealed.length);
      out.set(m, 0);
      out.set(sealed, m.length);
      return out;
    };
    expect(wrapRecoveryRow(sealed)).toEqual(tagged("v2"));
    expect(isRecoveryPayloadRow(tagged("v1"))).toBe(false);
    expect(unwrapRecoveryRow(tagged("v1"))).toBeNull();
    expect(RECOVERY_EXPORTER_LABEL).toBe("lakko/device-recovery/v2");
    expect(RECOVERY_PAYLOAD_LENGTH).toBe(48);
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
