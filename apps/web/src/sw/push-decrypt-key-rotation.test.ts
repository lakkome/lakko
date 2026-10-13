// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 section 3.3 — the BACKGROUND notification path
 * (`public/sw-push-decrypt.js`) never renders a `key-rotation` envelope.
 * The envelope means something only in the account's own DeviceGroup, which
 * the worker never reads; in a conversation journal it is stray bytes, and
 * unfiltered it would surface as its domain string plus the account's new
 * entropy as raw text. READ-ONLY (C-59). The fixture is the group-closed
 * test's.
 *
 * The classifier is the REAL core's `dispatchIncomingEnvelope` and the
 * envelope is composed by the real `composeKeyRotationEnvelope` over a real
 * supersession proof; the MLS group, the IDB reads and the DS are stand-ins.
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

const GROUP_HEX = "ab".repeat(32);
const GROUP_BYTES = new Uint8Array(32).fill(0xab);
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
    dispatchIncomingEnvelope: (bytes: Uint8Array) => realWasm.dispatchIncomingEnvelope(bytes),
    IncomingEnvelopeKind: realWasm.IncomingEnvelopeKind,
  };
}

function deps(messages: unknown[]) {
  const forbidden = { saveSnapshot: vi.fn(), saveMessageLog: vi.fn(), advanceCursor: vi.fn() };
  return {
    forbidden,
    deps: {
      readWakeBinding: async () => ({ devicePseudonymHex: "dd".repeat(32), accountId: "acct" }),
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

const wake = { deviceHex: "dd".repeat(32), groupHex: GROUP_HEX, pointer: 42 };

function keyRotation(): Uint8Array {
  const oldSeed = new Uint8Array(32).fill(1);
  const newSeed = new Uint8Array(32).fill(2);
  const proof = realWasm.signSupersessionProof(oldSeed, newSeed, 1_700_000_000n, 1_700_000_000n + 1_209_600n);
  return realWasm.composeKeyRotationEnvelope(1_700_000_000_000n, new Uint8Array(16).fill(9), proof);
}

describe("the background path never renders a key-rotation envelope", () => {
  it("the unfiltered envelope really would render as text (the defect)", () => {
    const parsed = wasmStub().parseBodyEnvelope(keyRotation()) as { kind: string; text: string };
    expect(parsed.kind).toBe("chat");
    expect(parsed.text).toContain("lakko/key-rotation/v1");
  });

  it("is the 192-byte envelope the core dispatches as KeyRotation", () => {
    const envelope = keyRotation();
    expect(envelope).toHaveLength(realWasm.keyRotationEnvelopeLen());
    expect(envelope).toHaveLength(192);
    const dispatched = realWasm.dispatchIncomingEnvelope(envelope);
    expect(dispatched.kind).toBe(realWasm.IncomingEnvelopeKind.KeyRotation);
    dispatched.free();
  });

  it("returns null for a lone key-rotation envelope", async () => {
    const { deps: d } = deps([raw(42, keyRotation())]);
    expect(await decryptPushPreview(wake, d)).toBeNull();
  });

  it("falls back to the older chat message when the envelope is NEWEST", async () => {
    const { deps: d } = deps([chat(41, "real message"), raw(42, keyRotation())]);
    expect((await decryptPushPreview(wake, d))?.body).toBe("real message");
  });

  it("still previews a legacy plaintext starting with `l`", async () => {
    const { deps: d } = deps([raw(42, new TextEncoder().encode("lunch at six?"))]);
    expect((await decryptPushPreview(wake, d))?.body).toBe("lunch at six?");
  });

  it("classifies without ever persisting (C-59)", async () => {
    const { deps: d, forbidden } = deps([raw(42, keyRotation())]);
    await decryptPushPreview(wake, d);
    expect(forbidden.saveSnapshot).not.toHaveBeenCalled();
    expect(forbidden.saveMessageLog).not.toHaveBeenCalled();
    expect(forbidden.advanceCursor).not.toHaveBeenCalled();
  });
});
