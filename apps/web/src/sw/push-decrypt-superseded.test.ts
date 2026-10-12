// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 R.5 / R.8 — nothing from a superseded sender acts, on the push
 * path too: a frame whose MLS-authenticated sender is bound to a key an
 * accepted reset retired (a removed device that still holds the old keys)
 * neither rings nor becomes a notification preview.
 *
 * The worker reads the retired keys from the plaintext mirror the app
 * writes (`src/push/superseded-mirror.ts`); here that read is the injected
 * `lookupSupersededKeys`.
 */
import { describe, expect, it, vi } from "vitest";

// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { decryptPushPreview } from "../../public/sw-push-decrypt.js";

const GROUP_HEX = "aa".repeat(32);
const GROUP_BYTES = new Uint8Array(32).fill(0xaa);
const ACCOUNT_ID = "YWNjb3VudC1pZA==";

function b64(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

const RETIRED = new Uint8Array(32).fill(0x0d); // the old identity's account key
const CURRENT = new Uint8Array(32).fill(0x0c); // a member in good standing
const RETIRED_B64 = b64(RETIRED);

/** Leaf 1 is a device of the retired key, leaf 2 of the current one. */
const MEMBERS = [
  { leafIndex: 0, signaturePublicKey: new Uint8Array(32).fill(1), accountKey: new Uint8Array(32).fill(0xaa) },
  { leafIndex: 1, signaturePublicKey: new Uint8Array(32).fill(2), accountKey: RETIRED },
  { leafIndex: 2, signaturePublicKey: new Uint8Array(32).fill(3), accountKey: CURRENT },
];

type Row = { id: number; leaf: number; body: Record<string, unknown> | "offer" };

/** A prost-shaped call offer: the `body` oneof tag 0x52. */
const OFFER = new Uint8Array([0x52, 0x08, 0x01, 0x12, 0x02, 0x68, 0x69]);

function deps(rows: Row[], overrides: Record<string, unknown> = {}) {
  const forbidden = { saveSnapshot: vi.fn(), saveMessageLog: vi.fn(), advanceCursor: vi.fn() };
  const leafOf = new Map<Uint8Array, number>();
  const messages = rows.map((r) => {
    const payload = r.body === "offer" ? OFFER.slice() : new TextEncoder().encode(JSON.stringify(r.body));
    leafOf.set(payload, r.leaf);
    return { id: r.id, payload, createdAt: new Date().toISOString() };
  });
  return {
    forbidden,
    deps: {
      readWakeBinding: async () => ({ devicePseudonymHex: "dd".repeat(32), accountId: ACCOUNT_ID }),
      resolveAccount: async () => ({ accountId: ACCOUNT_ID, groupIdBytes: GROUP_BYTES }),
      getDeviceKey: async () => ({}),
      getIdentitySeed: async () => new Uint8Array(64),
      loadSnapshot: async () => ({ snapshotBytes: new Uint8Array([1]), knownGroupIds: [] }),
      wasm: {
        Identity: { fromSeed: () => ({ restoreStorage: () => undefined, free: () => undefined }) },
        Group: {
          load: () => ({
            ownLeafIndex: 0,
            members: () => MEMBERS,
            processIncomingVerifyingRoles: (_i: unknown, payload: Uint8Array) => ({
              kind: "Application",
              applicationData: payload,
              senderLeaf: leafOf.get(payload),
            }),
            free: () => undefined,
          }),
        },
        parseBodyEnvelope: (bytes: Uint8Array) => ({
          ...(JSON.parse(new TextDecoder().decode(bytes)) as Record<string, unknown>),
          free: () => undefined,
        }),
        // Classifies the offer fixture; anything else is not a call signal.
        parseCallSignaling: (bytes: Uint8Array) => {
          if (bytes.length !== OFFER.length || bytes[0] !== 0x52) throw new Error("not a call signal");
          return {
            kind: "offer",
            callId: new Uint8Array(16).fill(7),
            mediaVideo: false,
            free: () => undefined,
          };
        },
      },
      dsFetchGroup: async () => ({ messages }),
      ...forbidden,
      ...overrides,
    },
  };
}

const wake = { deviceHex: "dd".repeat(32), groupHex: GROUP_HEX, pointer: 42 };
const chat = (text: string): Record<string, unknown> => ({ kind: "chat", text, attachmentCount: 0 });
const retiredKeys = { lookupSupersededKeys: async () => [RETIRED_B64] };

describe("a push from a superseded sender", () => {
  it("a chat row from a retired key posts nothing", async () => {
    const d = deps([{ id: 42, leaf: 1, body: chat("from the removed device") }], retiredKeys);
    await expect(decryptPushPreview(wake, d.deps)).resolves.toBeNull();
    for (const spy of Object.values(d.forbidden)) expect(spy).not.toHaveBeenCalled();
  });

  it("the same row from a member in good standing is previewed", async () => {
    const d = deps([{ id: 42, leaf: 2, body: chat("hello") }], retiredKeys);
    await expect(decryptPushPreview(wake, d.deps)).resolves.toMatchObject({ body: "hello" });
  });

  it("a retired sender's row never replaces a real one as the preview", async () => {
    const d = deps(
      [
        { id: 41, leaf: 2, body: chat("real") },
        { id: 42, leaf: 1, body: chat("from the removed device") },
      ],
      retiredKeys,
    );
    await expect(decryptPushPreview(wake, d.deps)).resolves.toMatchObject({ body: "real" });
  });

  it("with no retired key (or no mirror yet) every sender is previewed", async () => {
    const row: Row[] = [{ id: 42, leaf: 1, body: chat("still a member") }];
    await expect(decryptPushPreview(wake, deps(row).deps)).resolves.toMatchObject({ body: "still a member" });
    await expect(
      decryptPushPreview(wake, deps(row, { lookupSupersededKeys: async () => null }).deps),
    ).resolves.toMatchObject({ body: "still a member" });
    await expect(
      decryptPushPreview(wake, deps(row, { lookupSupersededKeys: async () => [] }).deps),
    ).resolves.toMatchObject({ body: "still a member" });
  });

  it("a mirror that cannot be read drops nothing (a notification too many, never one too few)", async () => {
    const d = deps([{ id: 42, leaf: 1, body: chat("kept") }], {
      lookupSupersededKeys: async () => {
        throw new Error("idb did not open");
      },
    });
    await expect(decryptPushPreview(wake, d.deps)).resolves.toMatchObject({ body: "kept" });
  });

  it("is read once per wake, for the bound account", async () => {
    const lookupSupersededKeys = vi.fn(async () => [RETIRED_B64]);
    const d = deps(
      [
        { id: 41, leaf: 1, body: chat("a") },
        { id: 42, leaf: 2, body: chat("b") },
      ],
      { lookupSupersededKeys },
    );
    await decryptPushPreview(wake, d.deps);
    expect(lookupSupersededKeys).toHaveBeenCalledTimes(1);
    expect(lookupSupersededKeys).toHaveBeenCalledWith(ACCOUNT_ID);
  });
});

describe("a call offer from a superseded sender", () => {
  it("does not ring", async () => {
    const d = deps([{ id: 42, leaf: 1, body: "offer" }], retiredKeys);
    const out = await decryptPushPreview(wake, d.deps);
    expect(out).toBeNull();
  });

  it("the same offer from a member in good standing rings", async () => {
    const d = deps([{ id: 42, leaf: 2, body: "offer" }], retiredKeys);
    const out = (await decryptPushPreview(wake, d.deps)) as { call?: { kind: string } } | null;
    expect(out?.call?.kind).toBe("ring");
  });
});
