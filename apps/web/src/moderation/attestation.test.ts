// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The attestation rules of the send and receive paths, against the REAL core
 * where the core decides (the wrapper, the verdict).
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  attestOutgoing,
  evidenceAtReceipt,
  evidenceBytes,
  evidenceFromPersisted,
  isServiceEnvelopeBytes,
  unwrapReceived,
  verdictOf,
} from "./attestation";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const groupId = new Uint8Array(32).fill(4);
const enc = (s: string): Uint8Array => new TextEncoder().encode(s);

describe("sending", () => {
  it("wraps a chat envelope and the receiver unwraps the same envelope", () => {
    const identity = wasm.Identity.fromSeed(new Uint8Array(32).fill(1));
    const envelope = enc('{"v":1,"text":"hello"}');
    const sealed = attestOutgoing(wasm, identity, groupId, envelope, true);
    expect(sealed.length).toBeGreaterThan(envelope.length);
    expect(Array.from(unwrapReceived(wasm, sealed))).toEqual(Array.from(envelope));
    expect(wasm.messageAttestation(sealed, groupId, identity.signaturePublicKey)).toBe("verified");
    // Another group's id, another key: the signature does not check out.
    expect(wasm.messageAttestation(sealed, new Uint8Array(32), identity.signaturePublicKey)).toBe("failed");
    identity.free();
  });

  it("never wraps a service envelope or a frame the caller does not attest", () => {
    const fake = {
      attestMessage: (): Uint8Array => {
        throw new Error("must not be called");
      },
    };
    const service = enc("lakko/profile/v1\u0000rest");
    expect(isServiceEnvelopeBytes(service)).toBe(true);
    expect(attestOutgoing(fake, null, groupId, service, true)).toBe(service);
    const typing = enc('{"v":1,"typing":true}');
    expect(attestOutgoing(fake, null, groupId, typing, false)).toBe(typing);
  });
});

describe("receiving", () => {
  it("leaves an unattested plaintext as it is", () => {
    const plain = enc('{"v":1,"text":"old"}');
    expect(Array.from(unwrapReceived(wasm, plain))).toEqual(Array.from(plain));
    expect(unwrapReceived(null, plain)).toBe(plain);
  });

  it("records the evidence at receipt, absent when there is no leaf key", () => {
    const identity = wasm.Identity.fromSeed(new Uint8Array(32).fill(2));
    const sealed = attestOutgoing(wasm, identity, groupId, enc('{"v":1,"text":"x"}'), true);
    const binding = new Uint8Array([1, 2, 3]);
    const e = evidenceAtReceipt(wasm, sealed, groupId, identity.signaturePublicKey, binding);
    expect(e?.attestation).toBe("verified");
    expect(evidenceAtReceipt(wasm, sealed, groupId, undefined, binding)).toBeUndefined();
    const plain = evidenceAtReceipt(wasm, enc('{"v":1}'), groupId, identity.signaturePublicKey, undefined);
    expect(plain?.attestation).toBe("absent");
    expect(plain?.bindingB64).toBeNull();
    // Persisted and read back, it carries the same bytes.
    const back = evidenceFromPersisted(JSON.parse(JSON.stringify(e)));
    const bytes = back === undefined ? null : evidenceBytes(back);
    expect(Array.from(bytes?.plaintext ?? [])).toEqual(Array.from(sealed));
    expect(Array.from(bytes?.binding ?? [])).toEqual([1, 2, 3]);
    identity.free();
  });

  it("reads a malformed stored row as no evidence and an unknown verdict as absent", () => {
    expect(evidenceFromPersisted(null)).toBeUndefined();
    expect(evidenceFromPersisted({ plaintextB64: 1 })).toBeUndefined();
    expect(verdictOf("weird")).toBe("absent");
  });
});
