// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0059 section 1, co-publisher keys — the properties the co-publisher
 * flow in `copublisher.ts` relies on, against the REAL wasm core.
 *
 * The owner appoints the key named in a binding only when the binding's
 * account key equals the joiner key it approved, which is the joiner's
 * `signaturePublicKey` (the external-join proposal's leaf key). So the
 * binding must name exactly that key, name the co-publisher key the joiner
 * will later sign with, and be refused when tampered with.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await init({ module_or_path: readFileSync(wasmPath) });
});

const CHANNEL = new Uint8Array(16).fill(0x42);

describe("co-publisher binding", () => {
  it("names the joiner's signature key and its co-publisher key", () => {
    const joiner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x51));
    const wire = wasm.composeChannelCoPublisherBinding(joiner, CHANNEL);
    const binding = wasm.decodeChannelCoPublisherBinding(wire);
    expect(Array.from(binding.accountPubkey)).toEqual(Array.from(joiner.signaturePublicKey));
    expect(Array.from(binding.channelId)).toEqual(Array.from(CHANNEL));
    const signer = wasm.channelCoPublisherSigner(joiner, CHANNEL);
    expect(Array.from(binding.copublisherPubkey)).toEqual(Array.from(signer.verifyingKey()));
    // Never the owner key the same account would derive.
    const owner = wasm.channelOwnerSigner(joiner, CHANNEL);
    expect(Array.from(signer.verifyingKey())).not.toEqual(Array.from(owner.verifyingKey()));
    // The proof header names the signing key: five fields.
    expect(signer.sign("GET", "/v1/channels/x/subscribers", "", new Uint8Array()).split(".")).toHaveLength(5);
  });

  it("refuses a tampered binding with a code", () => {
    const joiner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x52));
    const wire = wasm.composeChannelCoPublisherBinding(joiner, CHANNEL);
    const bad = wire.slice();
    bad[bad.length - 1] = (bad[bad.length - 1] ?? 0) ^ 1;
    let code: unknown;
    try {
      wasm.decodeChannelCoPublisherBinding(bad);
    } catch (err) {
      code = (err as { channelCoPublisherBindingError?: string }).channelCoPublisherBindingError;
    }
    expect(code).toBe("badSignature");
  });

  it("is not an MLS knock row, so the owner reads it before processKnock", () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x53));
    const joiner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x54));
    // ADR-0090: the group's leaf is a device bound to the owner's account.
    const device = wasm.Identity.fromSeedBound(new Uint8Array(64).fill(0x55), owner.ed25519Seed, 1_700_000_000n);
    const group = new wasm.PublisherGroup(device, owner.ed25519Seed, CHANNEL);
    const wire = wasm.composeChannelCoPublisherBinding(joiner, CHANNEL);
    expect(() => group.processKnock(device, wire)).toThrow();
  });

  it("builds the appointment path", () => {
    const path = wasm.channelCoPublisherPath(CHANNEL, new Uint8Array(32).fill(0xab));
    expect(path).toBe(`/v1/channels/${"42".repeat(16)}/copublishers/${"ab".repeat(32)}`);
    expect(wasm.channelMaxCoPublishers()).toBe(32);
  });
});
