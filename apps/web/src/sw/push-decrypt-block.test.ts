// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0099 sections 1 and 6 on the BACKGROUND notification path
 * (`public/sw-push-decrypt.js`): nothing from a blocked person becomes a
 * notification or a ring, and an attested message is previewed as the
 * envelope inside it.
 *
 * The block list is read the way the worker reads it, from the sync row the
 * app writes (`encodeSyncState` over the real `SyncedState`); the block rule
 * (`blockVerdict`) and the wrapper (`attestMessage` / `unwrapAttested`) are
 * the REAL core. The MLS group, the IDB reads and the DS are stand-ins, as in
 * `push-decrypt-profile.test.ts`. The sender check runs before any row is
 * classified, so it covers a call offer the same way as a message.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import initWasm, * as realWasm from "../../wasm/lakko_ffi_wasm";
import { encodeSyncState } from "../devices/sync/sync-state-store";
// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { attestedView, blockedKeysFromSyncStateRow, decryptPushPreview } from "../../public/sw-push-decrypt.js";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const GROUP_HEX = "aa".repeat(32);
const GROUP_BYTES = new Uint8Array(32).fill(0xaa);
const SENDER_KEY = new Uint8Array(32).fill(0x5e);
const SENDER_B64 = btoa(String.fromCharCode(...SENDER_KEY));

const chatBytes = (text: string): Uint8Array =>
  new TextEncoder().encode(JSON.stringify({ kind: "chat", text, attachmentCount: 0 }));
const row = (id: number, payload: Uint8Array) => ({ id, payload, createdAt: new Date(id * 1000).toISOString() });

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
    unwrapAttested: (bytes: Uint8Array) => realWasm.unwrapAttested(bytes),
    blockVerdict: (path: string, blocked: boolean) => realWasm.blockVerdict(path, blocked),
  };
}

function deps(messages: unknown[], blocked: readonly string[]) {
  return {
    readWakeBinding: async () => ({ accountId: "acct" }),
    lookupRoute: async () => ({
      accountId: "acct",
      groupIdB64: Buffer.from(GROUP_BYTES).toString("base64"),
      subIdHex: "dd".repeat(16),
    }),
    getDeviceKey: async () => ({}),
    getIdentitySeed: async () => new Uint8Array(64),
    loadSnapshot: async () => ({ snapshotBytes: new Uint8Array([1]), knownGroupIds: [] }),
    wasm: wasmStub(),
    dsFetchGroup: async () => ({ messages }),
    lookupSenderName: async () => "Test Sender",
    lookupBlockedKeys: async () => blocked,
  };
}

const wake = { subHex: "dd".repeat(16), groupHex: GROUP_HEX, pointer: 42 };

describe("a blocked person on the background path", () => {
  it("raises no notification", async () => {
    expect(await decryptPushPreview(wake, deps([row(42, chatBytes("hi"))], [SENDER_B64]))).toBeNull();
  });

  it("someone not blocked still notifies", async () => {
    const out = await decryptPushPreview(wake, deps([row(42, chatBytes("hi"))], []));
    expect(out?.body).toBe("hi");
  });

  it("reads the block list the app writes into its sync row", () => {
    const state = new realWasm.SyncedState();
    state.setBlocked(1n, SENDER_KEY, true, 5n);
    const payload = JSON.parse(encodeSyncState({ synced: state.toBytes(), pending: [] }));
    expect(blockedKeysFromSyncStateRow(payload, realWasm)).toEqual([SENDER_B64]);
    state.setBlocked(2n, SENDER_KEY, false, 6n);
    const after = JSON.parse(encodeSyncState({ synced: state.toBytes(), pending: [] }));
    expect(blockedKeysFromSyncStateRow(after, realWasm)).toEqual([]);
    expect(blockedKeysFromSyncStateRow({ v: 9 }, realWasm)).toEqual([]);
    expect(blockedKeysFromSyncStateRow(payload, null)).toEqual([]);
    state.free();
  });
});

describe("an attested message on the background path", () => {
  it("is previewed as the envelope inside it", async () => {
    const identity = realWasm.Identity.fromSeed(new Uint8Array(32).fill(3));
    const attested = realWasm.attestMessage(identity, GROUP_BYTES, chatBytes("signed hello"));
    identity.free();
    const out = await decryptPushPreview(wake, deps([row(42, attested)], []));
    expect(out?.body).toBe("signed hello");
  });

  it("leaves a frame with no application data as it is", () => {
    const frame = { kind: "Commit", applicationData: undefined, senderLeaf: 1 };
    expect(attestedView(realWasm, frame)).toBe(frame);
    const plain = { kind: "Application", applicationData: chatBytes("x"), senderLeaf: 1 };
    expect(attestedView(null, plain)).toBe(plain);
  });
});
