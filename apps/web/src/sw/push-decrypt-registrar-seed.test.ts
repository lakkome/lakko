// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 — the BACKGROUND notification path (`public/sw-push-decrypt.js`)
 * suppresses a registrar-seed envelope, READ-ONLY (C-59), exactly as it does
 * a shared pin (see `push-decrypt-pin.test.ts`, whose fixture this copies).
 *
 * The codec is the REAL core's `decodeRegistrarSeedEnvelope` and the envelope
 * is composed by the real `composeRegistrarSeedEnvelope`; the MLS group, the
 * IDB reads and the DS are stand-ins.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as realWasm from "../../wasm/lakko_ffi_wasm";
// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { decryptPushPreview } from "../../public/sw-push-decrypt.js";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const GROUP_HEX = "ac".repeat(32);
const GROUP_BYTES = new Uint8Array(32).fill(0xac);
const SENDER_KEY = new Uint8Array(32).fill(0x5e);

const chat = (id: number, text: string) => ({
  id,
  payload: new TextEncoder().encode(JSON.stringify({ kind: "chat", text, attachmentCount: 0 })),
  createdAt: new Date(id * 1000).toISOString(),
});
const raw = (id: number, payload: Uint8Array) => ({ id, payload, createdAt: new Date(id * 1000).toISOString() });

function wasmStub() {
  return {
    Identity: { fromSeed: () => ({ restoreStorage: () => undefined, free: () => undefined }) },
    Group: {
      load: () => ({
        processIncomingVerifyingRoles: (_i: unknown, payload: Uint8Array) => ({
          kind: "Application",
          applicationData: payload,
          senderLeaf: 2,
        }),
        members: () => [{ leafIndex: 2, signaturePublicKey: SENDER_KEY }],
        free: () => undefined,
      }),
    },
    parseBodyEnvelope: (bytes: Uint8Array) => {
      const text = new TextDecoder().decode(bytes);
      try {
        return { ...(JSON.parse(text) as object), free: () => undefined };
      } catch {
        return { kind: "chat", text, attachmentCount: 0, free: () => undefined };
      }
    },
    decodeRegistrarSeedEnvelope: (bytes: Uint8Array) => realWasm.decodeRegistrarSeedEnvelope(bytes),
    registrarSeedEnvelopeLen: () => realWasm.registrarSeedEnvelopeLen(),
  };
}

function deps(messages: unknown[]) {
  const forbidden = { saveSnapshot: vi.fn(), saveMessageLog: vi.fn(), advanceCursor: vi.fn() };
  return {
    forbidden,
    deps: {
      resolveAccount: async () => ({ accountId: "acct", groupIdBytes: GROUP_BYTES }),
      getDeviceKey: async () => ({}),
      getIdentitySeed: async () => new Uint8Array(64),
      loadSnapshot: async () => ({ snapshotBytes: new Uint8Array([1]), knownGroupIds: [] }),
      wasm: wasmStub(),
      dsFetchGroup: async () => ({ messages }),
      ...forbidden,
    },
  };
}

const wake = { deviceHex: null, groupHex: GROUP_HEX, pointer: 42 };

function envelope(): Uint8Array {
  const member = new realWasm.Identity("member");
  try {
    return realWasm.composeRegistrarSeedEnvelope(
      GROUP_BYTES,
      member.signaturePublicKey,
      member.x25519PublicKey,
      1,
      new Uint8Array(32).fill(0x33),
    );
  } finally {
    member.free();
  }
}

describe("the background path never previews a registrar seed", () => {
  it("is the one fixed length the codec advertises", () => {
    expect(envelope().length).toBe(realWasm.registrarSeedEnvelopeLen());
    expect(envelope()[0]).toBe(0x6c);
  });

  it("returns null for a lone registrar seed", async () => {
    const { deps: d } = deps([raw(42, envelope())]);
    expect(await decryptPushPreview(wake, d)).toBeNull();
  });

  it("falls back to the older chat message when the seed is NEWEST", async () => {
    const { deps: d } = deps([chat(41, "real message"), raw(42, envelope())]);
    expect((await decryptPushPreview(wake, d))?.body).toBe("real message");
  });

  it("classifies without ever persisting (C-59)", async () => {
    const { deps: d, forbidden } = deps([raw(42, envelope())]);
    await decryptPushPreview(wake, d);
    expect(forbidden.saveSnapshot).not.toHaveBeenCalled();
    expect(forbidden.saveMessageLog).not.toHaveBeenCalled();
    expect(forbidden.advanceCursor).not.toHaveBeenCalled();
  });
});
