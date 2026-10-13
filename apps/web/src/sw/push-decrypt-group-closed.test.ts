// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 phase 2 — the BACKGROUND notification path
 * (`public/sw-push-decrypt.js`) suppresses a group-closed envelope,
 * READ-ONLY (C-59), exactly as it does the group name (see
 * `push-decrypt-group-name.test.ts`, whose fixture this copies).
 *
 * The codec is the REAL core's `decodeGroupClosedEnvelope` and the
 * envelopes are composed by the real `composeGroupClosedEnvelope`; the MLS
 * group, the IDB reads and the DS are stand-ins.
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
    decodeGroupClosedEnvelope: (bytes: Uint8Array) => realWasm.decodeGroupClosedEnvelope(bytes),
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
const SUCCESSOR = new Uint8Array(32).fill(0x77);
const groupClosed = (successor: Uint8Array | undefined = SUCCESSOR) =>
  realWasm.composeGroupClosedEnvelope(5n, successor);

describe("the background path never previews a group-closed notice", () => {
  it("the unfiltered envelope really would render as text (the defect)", () => {
    const parsed = wasmStub().parseBodyEnvelope(groupClosed()) as { kind: string; text: string };
    expect(parsed.kind).toBe("chat");
    expect(parsed.text).toContain("lakko/group-closed/v1");
  });

  it("returns null for a lone group-closed notice", async () => {
    const { deps: d } = deps([raw(42, groupClosed())]);
    expect(await decryptPushPreview(wake, d)).toBeNull();
  });

  it("falls back to the older chat message when a notice is NEWEST", async () => {
    const { deps: d } = deps([chat(41, "real message"), raw(42, groupClosed())]);
    expect((await decryptPushPreview(wake, d))?.body).toBe("real message");
  });

  it("suppresses a malformed notice rather than rendering it", async () => {
    const good = groupClosed();
    const bad = new Uint8Array(good.length + 1);
    bad.set(good, 0);
    bad[good.length] = 0x41;
    const { deps: d } = deps([raw(42, bad)]);
    expect(await decryptPushPreview(wake, d)).toBeNull();
  });

  it("a close without a successor is a notice too", async () => {
    const { deps: d } = deps([raw(42, groupClosed(undefined))]);
    expect(await decryptPushPreview(wake, d)).toBeNull();
  });

  it("still previews a legacy plaintext starting with `l`", async () => {
    const { deps: d } = deps([raw(42, new TextEncoder().encode("lunch at six?"))]);
    expect((await decryptPushPreview(wake, d))?.body).toBe("lunch at six?");
  });

  it("classifies without ever persisting (C-59)", async () => {
    const { deps: d, forbidden } = deps([raw(42, groupClosed())]);
    await decryptPushPreview(wake, d);
    expect(forbidden.saveSnapshot).not.toHaveBeenCalled();
    expect(forbidden.saveMessageLog).not.toHaveBeenCalled();
    expect(forbidden.advanceCursor).not.toHaveBeenCalled();
  });
});
