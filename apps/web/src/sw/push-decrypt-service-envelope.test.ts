// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The BACKGROUND notification path (`public/sw-push-decrypt.js`) never
 * previews a `lakko/…` service envelope it does not recognise: a newer version
 * of a known kind (the v3 profile of the 2026-09-29 live walk) or a kind this
 * build has never heard of.
 *
 * The chat parser is the REAL core's `parseBodyEnvelope`, which now classifies
 * such plaintext as `service-envelope` instead of degrading it to lossy-UTF-8
 * chat text, and the profile codec is the real `decodeProfileEnvelope`. The
 * MLS group, the IDB reads and the DS are stand-ins, as in
 * `push-decrypt.test.ts`.
 *
 * Discrimination (run): with the `service-envelope` arm removed from
 * `previewFromProcessed`, the unknown-kind cases still pass, because that
 * function only ever previews `chat` and `edit`; the arm is explicit so a later
 * edit that previews "anything with text" cannot regress it. What the tests
 * pin is the end-to-end behaviour: with the core's classification replaced by
 * the old lossy fallback (a `chat` body carrying the decoded bytes), two tests
 * go red, among them "returns null for a lone unknown service envelope" with
 * `expected { title: 'Lakko', … } to be null`.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as realWasm from "../../wasm/lakko_ffi_wasm";
// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { decryptPushPreview } from "../../public/sw-push-decrypt.js";

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
const raw = (id: number, payload: Uint8Array) => ({ id, payload, createdAt: new Date(id * 1000).toISOString() });

function v3Profile(): Uint8Array {
  const domain = enc.encode("lakko/profile/v1\0");
  const out = new Uint8Array(domain.length + 1 + 40);
  out.set(domain, 0);
  out[domain.length] = 0x03;
  out.set(enc.encode("Zed"), domain.length + 12);
  return out;
}

function unknownKind(): Uint8Array {
  const domain = enc.encode("lakko/future-kind/v1\0");
  const out = new Uint8Array(domain.length + 4);
  out.set(domain, 0);
  out.set([0x01, 0x00, 0xff, 0x10], domain.length);
  return out;
}

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

describe("the background path never previews an unrecognised service envelope", () => {
  it("the core classifies both shapes as a service envelope", () => {
    expect(realWasm.parseBodyEnvelope(v3Profile()).kind).toBe("service-envelope");
    expect(realWasm.parseBodyEnvelope(unknownKind()).kind).toBe("service-envelope");
  });

  it("returns null for a lone v3 profile", async () => {
    expect(await decryptPushPreview(wake, deps([raw(42, v3Profile())]))).toBeNull();
  });

  it("returns null for a lone unknown service envelope", async () => {
    expect(await decryptPushPreview(wake, deps([raw(42, unknownKind())]))).toBeNull();
  });

  it("falls back to the older chat message when a service envelope is NEWEST", async () => {
    const out = await decryptPushPreview(
      wake,
      deps([chat(40, "real message"), raw(41, v3Profile()), raw(42, unknownKind())]),
    );
    expect(out?.body).toBe("real message");
  });

  it("still previews an ordinary message starting with `lakko`", async () => {
    const out = await decryptPushPreview(wake, deps([raw(42, enc.encode("lakko is great"))]));
    expect(out?.body).toBe("lakko is great");
  });
});
