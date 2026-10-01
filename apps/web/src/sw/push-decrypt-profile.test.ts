// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 — the BACKGROUND notification path (`public/sw-push-decrypt.js`)
 * suppresses a profile envelope, and titles a notification with a name this
 * device already knows, READ-ONLY (C-59).
 *
 * The profile codec is the REAL core's `decodeProfileEnvelope` (and the
 * envelopes are composed by the real `composeProfileEnvelope`); the MLS group,
 * the IDB reads and the DS are stand-ins, as in `push-decrypt.test.ts`.
 *
 * ## Discrimination (verified by running the broken variant)
 *
 * With arm 4 removed from `isServiceEnvelope`, three cases fail: "returns null
 * for a lone profile" (`expected { title: 'Lakko', … } to be null`), "falls
 * back to the older chat message when a profile is NEWEST" (the body is
 * `lakko/profile/v1…` instead of `real message`), and the malformed case.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as realWasm from "../../wasm/lakko_ffi_wasm";
// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { decryptPushPreview, senderNameFromProfileRow } from "../../public/sw-push-decrypt.js";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const GROUP_HEX = "aa".repeat(32);
const GROUP_BYTES = new Uint8Array(32).fill(0xaa);
const SENDER_KEY = new Uint8Array(32).fill(0x5e);
const SENDER_B64 = btoa(String.fromCharCode(...SENDER_KEY));

const chat = (id: number, text: string) => ({
  id,
  payload: new TextEncoder().encode(JSON.stringify({ kind: "chat", text, attachmentCount: 0 })),
  createdAt: new Date(id * 1000).toISOString(),
});
const raw = (id: number, payload: Uint8Array) => ({ id, payload, createdAt: new Date(id * 1000).toISOString() });

/** Mock core: the group hands payloads back from leaf 2 (= SENDER_KEY); the
 *  chat parser models the real lossy fallback; the profile codec is REAL. */
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
    decodeProfileEnvelope: (bytes: Uint8Array) => realWasm.decodeProfileEnvelope(bytes),
  };
}

function deps(messages: unknown[], overrides: Record<string, unknown> = {}) {
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
      ...overrides,
    },
  };
}

const wake = { deviceHex: "dd".repeat(32), groupHex: GROUP_HEX, pointer: 42 };
const profile = (name = "Zed Unique Name") => realWasm.composeProfileEnvelope(name, "zed", 5n);

describe("the background path never previews a profile", () => {
  it("the unfiltered profile really would render as text (the defect)", () => {
    const parsed = wasmStub().parseBodyEnvelope(profile()) as { kind: string; text: string };
    expect(parsed.kind).toBe("chat");
    expect(parsed.text).toContain("Zed Unique Name");
  });

  it("returns null for a lone profile", async () => {
    const { deps: d } = deps([raw(42, profile())]);
    expect(await decryptPushPreview(wake, d)).toBeNull();
  });

  it("falls back to the older chat message when a profile is NEWEST", async () => {
    const { deps: d } = deps([chat(41, "real message"), raw(42, profile())]);
    const out = await decryptPushPreview(wake, d);
    expect(out?.body).toBe("real message");
  });

  it("suppresses a malformed profile rather than rendering it", async () => {
    const good = profile();
    const bad = new Uint8Array(good.length + 1);
    bad.set(good, 0);
    bad[good.length] = 0x41;
    const { deps: d } = deps([raw(42, bad)]);
    expect(await decryptPushPreview(wake, d)).toBeNull();
  });

  it("still previews a legacy plaintext starting with `l`", async () => {
    const { deps: d } = deps([raw(42, new TextEncoder().encode("lunch at six?"))]);
    const out = await decryptPushPreview(wake, d);
    expect(out?.body).toBe("lunch at six?");
  });

  it("classifies without ever persisting (C-59)", async () => {
    const { deps: d, forbidden } = deps([raw(42, profile())]);
    await decryptPushPreview(wake, d);
    expect(forbidden.saveSnapshot).not.toHaveBeenCalled();
    expect(forbidden.saveMessageLog).not.toHaveBeenCalled();
    expect(forbidden.advanceCursor).not.toHaveBeenCalled();
  });
});

describe("the notification title uses a name already known", () => {
  it("titles with the name the app stored for the sender's key", async () => {
    const lookupSenderName = vi.fn(async (_a: string, _k: unknown, key: string) =>
      key === SENDER_B64 ? "Alice Petrova" : null,
    );
    const { deps: d } = deps([chat(42, "hi")], { lookupSenderName });
    const out = await decryptPushPreview(wake, d);
    expect(out?.title).toBe("Alice Petrova");
    expect(lookupSenderName).toHaveBeenCalledWith("acct", {}, SENDER_B64);
  });

  it("keeps the generic title when nothing is known or the lookup throws", async () => {
    const { deps: unknown } = deps([chat(42, "hi")], { lookupSenderName: async () => null });
    expect((await decryptPushPreview(wake, unknown))?.title).toBe("Lakko");
    const { deps: throwing } = deps([chat(42, "hi")], {
      lookupSenderName: async () => {
        throw new Error("idb");
      },
    });
    expect((await decryptPushPreview(wake, throwing))?.title).toBe("Lakko");
  });
});

describe("senderNameFromProfileRow — rename, profile name, verified username", () => {
  const row = (over: Record<string, unknown> = {}) => ({
    v: 1,
    p: { [SENDER_B64]: { d: "Alice Petrova", u: "alice", t: "5" } },
    vf: { [`${SENDER_B64}|alice`]: { s: "verified", c: 1 } },
    r: {},
    ...over,
  });

  it("a local rename wins", () => {
    expect(senderNameFromProfileRow(row({ r: { [SENDER_B64]: "Mum" } }), SENDER_B64)).toBe("Mum");
  });

  it("then the profile name", () => {
    expect(senderNameFromProfileRow(row(), SENDER_B64)).toBe("Alice Petrova");
  });

  it("then a VERIFIED username only", () => {
    const noName = { [SENDER_B64]: { d: null, u: "alice", t: "5" } };
    expect(senderNameFromProfileRow(row({ p: noName }), SENDER_B64)).toBe("@alice");
    expect(
      senderNameFromProfileRow(
        row({ p: noName, vf: { [`${SENDER_B64}|alice`]: { s: "mismatch", c: 1 } } }),
        SENDER_B64,
      ),
    ).toBeNull();
    expect(senderNameFromProfileRow(row({ p: noName, vf: {} }), SENDER_B64)).toBeNull();
  });

  it("unknown sender or foreign schema is null", () => {
    expect(senderNameFromProfileRow(row(), "other")).toBeNull();
    expect(senderNameFromProfileRow({ v: 2 }, SENDER_B64)).toBeNull();
    expect(senderNameFromProfileRow(null, SENDER_B64)).toBeNull();
  });
});
