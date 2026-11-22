// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/message-actions.md` section 9 on the BACKGROUND notification path
 * (`public/sw-push-decrypt.js`): a deletion never makes a notification, and a
 * notification never describes a deleted message. When the newest thing a
 * wake finds is a deletion of a message in the same batch, the notification
 * describes the newest message left, or nothing. The chat parser is the REAL
 * core's `parseBodyEnvelope`; the group and the stores are stand-ins, as in
 * `push-decrypt-service-envelope.test.ts`.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as realWasm from "../../wasm/lakko_ffi_wasm";
// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { decryptPushPreview, decryptPushWake } from "../../public/sw-push-decrypt.js";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const GROUP_HEX = "aa".repeat(32);
const GROUP_BYTES = new Uint8Array(32).fill(0xaa);
const enc = new TextEncoder();

const chat = (id: number, text: string) => ({
  id,
  payload: enc.encode(JSON.stringify({ v: 1, text })),
  createdAt: new Date(id * 1000).toISOString(),
});

/** Real chat parser and profile codec; stand-in group. */
function wasmStub(parseBodyEnvelope: (b: Uint8Array) => unknown = realWasm.parseBodyEnvelope) {
  return {
    Identity: { fromSeed: () => ({ restoreStorage: () => undefined, free: () => undefined }) },
    Group: {
      load: () => ({
        processIncomingVerifyingRoles: (_i: unknown, payload: Uint8Array) => ({
          kind: "Application",
          applicationData: payload,
          senderLeaf: 2,
        }),
        members: () => [{ leafIndex: 2, signaturePublicKey: new Uint8Array(32).fill(7) }],
        free: () => undefined,
      }),
    },
    parseBodyEnvelope,
    decodeProfileEnvelope: (bytes: Uint8Array) => realWasm.decodeProfileEnvelope(bytes),
  };
}

function deps(messages: unknown[]) {
  return {
    readWakeBinding: async () => ({ accountId: "acct" }),
    // A sender always has a name on this device (the mirror the app writes).
    lookupSenderName: async () => "Test Sender",
    lookupRoute: async () => ({ accountId: "acct", groupIdB64: Buffer.from(GROUP_BYTES).toString("base64"), subIdHex: "dd".repeat(16) }),
    getDeviceKey: async () => ({}),
    getIdentitySeed: async () => new Uint8Array(64),
    loadSnapshot: async () => ({ snapshotBytes: new Uint8Array([1]), knownGroupIds: [] }),
    wasm: wasmStub(),
    dsFetchGroup: async () => ({ messages }),
  };
}

const wake = { subHex: "dd".repeat(16), groupHex: GROUP_HEX, pointer: 42 };

const op = (id: number, fields: Record<string, unknown>) => ({
  id,
  payload: enc.encode(JSON.stringify({ v: 1, text: "", ...fields })),
  createdAt: new Date(id * 1000).toISOString(),
});

describe("a deletion in the batch never reaches a notification", () => {
  it("the core parses the delete op this test sends", () => {
    const parsed = realWasm.parseBodyEnvelope(enc.encode(JSON.stringify({ v: 1, text: "", delete_of: 40 })));
    expect(parsed.kind).toBe("delete");
  });

  it("a message and its deletion in one batch show nothing", async () => {
    expect(await decryptPushPreview(wake, deps([chat(40, "gone"), op(41, { delete_of: 40 })]))).toBeNull();
  });

  it("falls back to the newest message that is not deleted", async () => {
    const out = await decryptPushPreview(
      wake,
      deps([chat(39, "still here"), chat(40, "gone"), op(41, { delete_of: 40 })]),
    );
    expect(out?.body).toBe("still here");
  });

  it("a deletion of a message outside the batch changes nothing", async () => {
    const out = await decryptPushPreview(wake, deps([chat(40, "kept"), op(41, { delete_of: 12 })]));
    expect(out?.body).toBe("kept");
  });

  it("an edit of a message deleted in the same batch shows nothing", async () => {
    expect(
      await decryptPushPreview(
        wake,
        deps([op(40, { edit_of: 12, text: "edited" }), op(41, { delete_of: 12 })]),
      ),
    ).toBeNull();
  });

  it("the wake names the message the preview stands for", async () => {
    const outcome = await decryptPushWake(wake, deps([chat(39, "still here"), chat(40, "gone"), op(41, { delete_of: 40 })]));
    expect(outcome.previewMessageId).toBe("39");
  });
});
