// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Security review 2026-11-21 - an account X25519 key from the directory is
 * sealed to only when its binding to the signature key verifies.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it, vi } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import { DiscoveryClient } from "./client";
import type { IdentityUploadRequest } from "./wire";
import { boundX25519With, type BindingFields } from "./x25519-binding";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const verify = (b: Uint8Array, e: Uint8Array, x: Uint8Array): boolean => wasm.verifyX25519Binding(b, e, x);

function bundleOf(identity: wasm.Identity, binding: Uint8Array | null): BindingFields {
  return {
    signature_pubkey: base64Encode(identity.signaturePublicKey),
    x25519_pubkey: base64Encode(identity.x25519PublicKey),
    ...(binding === null ? {} : { x25519_binding: base64Encode(binding) }),
  };
}

describe("the bound X25519 key", () => {
  it("is the bundle's key when the account signed it", () => {
    const id = wasm.Identity.fromSeed(new Uint8Array(32).fill(1));
    expect(wasm.x25519BindingLen()).toBe(64);
    const key = boundX25519With(verify, bundleOf(id, wasm.signX25519Binding(id)));
    expect(key).not.toBeNull();
    expect([...(key ?? [])]).toEqual([...id.x25519PublicKey]);
  });

  it("is refused when the binding is absent, empty or malformed", () => {
    const id = wasm.Identity.fromSeed(new Uint8Array(32).fill(2));
    expect(boundX25519With(verify, bundleOf(id, null))).toBeNull();
    expect(boundX25519With(verify, { ...bundleOf(id, null), x25519_binding: "" })).toBeNull();
    expect(boundX25519With(verify, { ...bundleOf(id, null), x25519_binding: "!!not base64!!" })).toBeNull();
    expect(boundX25519With(verify, bundleOf(id, new Uint8Array(64)))).toBeNull();
  });

  it("is refused when a directory pairs the person's signature key with another X25519 key", () => {
    const person = wasm.Identity.fromSeed(new Uint8Array(32).fill(3));
    const attacker = wasm.Identity.fromSeed(new Uint8Array(32).fill(4));
    const swapped: BindingFields = {
      signature_pubkey: base64Encode(person.signaturePublicKey),
      x25519_pubkey: base64Encode(attacker.x25519PublicKey),
      // The attacker's own binding, or the person's: neither verifies.
      x25519_binding: base64Encode(wasm.signX25519Binding(attacker)),
    };
    expect(boundX25519With(verify, swapped)).toBeNull();
    expect(boundX25519With(verify, { ...swapped, x25519_binding: base64Encode(wasm.signX25519Binding(person)) })).toBeNull();
  });

  it("a verifier that throws refuses", () => {
    const id = wasm.Identity.fromSeed(new Uint8Array(32).fill(5));
    expect(
      boundX25519With(() => {
        throw new Error("x");
      }, bundleOf(id, wasm.signX25519Binding(id))),
    ).toBeNull();
  });
});

describe("the identity upload", () => {
  it("carries the 64-byte binding in standard base64", async () => {
    const client = new DiscoveryClient("http://localhost:0");
    let body: IdentityUploadRequest | null = null;
    vi.spyOn(client, "identityUpload").mockImplementation(async (b) => {
      body = b;
      return { ok: true };
    });
    const id = wasm.Identity.fromSeed(new Uint8Array(32).fill(6));
    const binding = wasm.signX25519Binding(id);
    await client.uploadIdentityBundle(new Uint8Array(16).fill(1), id.signaturePublicKey, id.x25519PublicKey, binding);
    expect((body as IdentityUploadRequest | null)?.x25519_binding).toBe(base64Encode(binding));
  });

  it("refuses a binding of the wrong length before anything is sent", async () => {
    const client = new DiscoveryClient("http://localhost:0");
    const spy = vi.spyOn(client, "identityUpload").mockResolvedValue({ ok: true });
    await expect(
      client.uploadIdentityBundle(new Uint8Array(16), new Uint8Array(32), new Uint8Array(32), new Uint8Array(63)),
    ).rejects.toBeDefined();
    expect(spy).not.toHaveBeenCalled();
  });
});
