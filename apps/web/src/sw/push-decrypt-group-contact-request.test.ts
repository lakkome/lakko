// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/who-can-find-me.md` section 6.4 — the background notification
 * path suppresses a contact request sealed inside a group, READ-ONLY (C-59),
 * exactly as it does a registrar seed (`push-decrypt-registrar-seed.test.ts`,
 * whose fixture this copies). The envelope and its recogniser are the REAL
 * core's; the MLS group, the IDB reads and the DS are stand-ins.
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
    isGroupContactRequestEnvelope: (bytes: Uint8Array) => realWasm.isGroupContactRequestEnvelope(bytes),
    groupContactRequestEnvelopeMinLen: () => realWasm.groupContactRequestEnvelopeMinLen(),
    groupContactRequestEnvelopeMaxLen: () => realWasm.groupContactRequestEnvelopeMaxLen(),
  };
}

function deps(messages: unknown[]) {
  const forbidden = { saveSnapshot: vi.fn(), saveMessageLog: vi.fn(), advanceCursor: vi.fn() };
  return {
    forbidden,
    deps: {
      readWakeBinding: async () => ({ accountId: "acct" }),
      lookupSenderName: async () => "Test Sender",
      lookupRoute: async () => ({ accountId: "acct", groupIdB64: Buffer.from(GROUP_BYTES).toString("base64"), subIdHex: "dd".repeat(16) }),
      getDeviceKey: async () => ({}),
      getIdentitySeed: async () => new Uint8Array(64),
      loadSnapshot: async () => ({ snapshotBytes: new Uint8Array([1]), knownGroupIds: [] }),
      wasm: wasmStub(),
      dsFetchGroup: async () => ({ messages }),
      ...forbidden,
    },
  };
}

const wake = { subHex: "dd".repeat(16), groupHex: GROUP_HEX, pointer: 42 };

/** A real envelope: an ordinary contact request sealed to a recipient. */
function envelope(): Uint8Array {
  const sender = new realWasm.Identity("sender");
  const recipient = new realWasm.Identity("recipient");
  try {
    const request = realWasm.ContactRequest.compose(
      sender.ed25519Seed,
      new Uint8Array(16).fill(1),
      recipient.x25519PublicKey,
      new Uint8Array(32).fill(2),
      1n,
      new Uint8Array(0),
      new Uint8Array(0),
      undefined,
      new Uint8Array(0),
      new Uint8Array(0),
      new Uint8Array(0),
    );
    return realWasm.composeGroupContactRequestEnvelope(recipient.x25519PublicKey, request);
  } finally {
    sender.free();
    recipient.free();
  }
}

describe("the background path never previews a contact request sealed inside a group", () => {
  it("returns null for a lone request", async () => {
    const { deps: d } = deps([raw(42, envelope())]);
    expect(await decryptPushPreview(wake, d)).toBeNull();
  });

  it("falls back to the older chat message when the request is NEWEST", async () => {
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
