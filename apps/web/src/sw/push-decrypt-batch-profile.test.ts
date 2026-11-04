// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 — a profile and a message in ONE wake: the worker drops the
 * profile as a preview, but names its sender with it for the rest of the
 * batch, in memory only (C-59), keyed as the app keys received profiles: the
 * person key resolved at receipt (the account key) and the leaf key.
 *
 * The profile envelopes are composed and decoded by the REAL core; the name
 * is read the way `sw.js` reads it (`senderNameFromProfileRow` over a row the
 * app wrote through the real `ProfileStore` and `encodeProfileRow`). The MLS
 * group and the DS are stand-ins, as in `push-decrypt.test.ts`.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as realWasm from "../../wasm/lakko_ffi_wasm";
// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { decryptPushPreview, senderNameFromProfileRow } from "../../public/sw-push-decrypt.js";
import { MemoryProfileBackend, ProfileStore } from "../profile/profile-store";
import { encodeProfileRow } from "../profile/profile-idb";
import { sniffProfile } from "../profile/profile-envelope";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const GROUP_HEX = "aa".repeat(32);
const GROUP_BYTES = new Uint8Array(32).fill(0xaa);
const OWN = new Uint8Array(32).fill(0x0b);
const LEAF = new Uint8Array(32).fill(0x31);
const ACCOUNT = new Uint8Array(32).fill(0x32);
const b64 = (b: Uint8Array): string => btoa(String.fromCharCode(...b));
const wake = { subHex: "dd".repeat(16), groupHex: GROUP_HEX, pointer: 42 };

const at = (id: number) => new Date(id * 1000).toISOString();
const profileRow = (id: number, name: string, stamp: bigint, leaf = 2) => ({
  id,
  leaf,
  payload: realWasm.composeProfileEnvelope(name, "vg3", stamp),
  createdAt: at(id),
});
const chatRow = (id: number, text: string, leaf = 2) => ({
  id,
  leaf,
  payload: new TextEncoder().encode(JSON.stringify({ kind: "chat", text, attachmentCount: 0 })),
  createdAt: at(id),
});
const reactionRow = (id: number, leaf = 2) => ({
  id,
  leaf,
  payload: new TextEncoder().encode(JSON.stringify({ kind: "reaction", emoji: "❤", target: 7 })),
  createdAt: at(id),
});

/** A group (ADR-0062 kind 2): this device at leaf 1, VG3 at leaf 2 bound to
 *  its account key, unless `bound` is false. Each row carries its sender's
 *  leaf, which the stand-in hands back as the MLS sender. */
function wasmStub(opts: { bound?: boolean; oneToOne?: boolean } = {}) {
  const leafOf = new Map<Uint8Array, number>();
  return {
    leafOf,
    wasm: {
      GroupKind: { OneToOne: 1, Group: 2 },
      Identity: { fromSeed: () => ({ restoreStorage: () => undefined, free: () => undefined }) },
      Group: {
        load: () => ({
          processIncomingVerifyingRoles: (_i: unknown, payload: Uint8Array) => ({
            kind: "Application",
            applicationData: payload,
            senderLeaf: leafOf.get(payload) ?? 2,
          }),
          ownLeafIndex: () => 1,
          members: () => [
            { leafIndex: 1, signaturePublicKey: OWN },
            opts.bound === false
              ? { leafIndex: 2, signaturePublicKey: LEAF }
              : { leafIndex: 2, signaturePublicKey: LEAF, accountKey: ACCOUNT },
          ],
          groupKind: () => (opts.oneToOne === true ? 1 : 2),
          free: () => undefined,
        }),
      },
      parseBodyEnvelope: (bytes: Uint8Array) => {
        const text = new TextDecoder().decode(bytes);
        try {
          const p = JSON.parse(text) as { kind: string };
          return p.kind === "reaction"
            ? { ...p, reactionTargetAuthor: OWN, free: () => undefined }
            : { ...p, free: () => undefined };
        } catch {
          return { kind: "chat", text, attachmentCount: 0, free: () => undefined };
        }
      },
      decodeProfileEnvelope: (bytes: Uint8Array) => realWasm.decodeProfileEnvelope(bytes),
    },
  };
}

/** The row the app wrote, read as `sw.js` reads it. */
function rowReader(encoded: string | null) {
  const forbidden = { saveSnapshot: vi.fn(), saveMessageLog: vi.fn(), advanceCursor: vi.fn() };
  return {
    forbidden,
    lookupSenderName: async (_a: string, _k: unknown, key: string, batch?: unknown) =>
      encoded === null ? null : senderNameFromProfileRow(JSON.parse(encoded), key, batch),
  };
}

async function run(
  rows: Array<{ id: number; leaf: number; payload: Uint8Array; createdAt: string }>,
  encodedRow: string | null,
  stub = wasmStub(),
) {
  for (const r of rows) stub.leafOf.set(r.payload, r.leaf);
  const reader = rowReader(encodedRow);
  const errors = vi.spyOn(console, "error").mockImplementation(() => undefined);
  try {
    const out = await decryptPushPreview(wake, {
      readWakeBinding: async () => ({ accountId: "acct" }),
      lookupRoute: async () => ({ accountId: "acct", groupIdB64: Buffer.from(GROUP_BYTES).toString("base64"), subIdHex: "dd".repeat(16) }),
      getDeviceKey: async () => ({}),
      getIdentitySeed: async () => new Uint8Array(64),
      loadSnapshot: async () => ({ snapshotBytes: new Uint8Array([1]), knownGroupIds: [] }),
      wasm: stub.wasm,
      dsFetchGroup: async () => ({ messages: rows.map(({ leaf: _l, ...m }) => m) }),
      lookupSenderName: reader.lookupSenderName,
      lookupGroupName: async () => "Test VG's",
      ...reader.forbidden,
    });
    expect(reader.forbidden.saveSnapshot).not.toHaveBeenCalled();
    expect(reader.forbidden.saveMessageLog).not.toHaveBeenCalled();
    expect(reader.forbidden.advanceCursor).not.toHaveBeenCalled();
    return { out, errors: errors.mock.calls.length };
  } finally {
    errors.mockRestore();
  }
}

/** A row the app wrote through the real store: `profiles` filed by `ingest`
 *  under the keys given, `renames` beside them. */
async function appRow(
  profiles: Array<[string, string, bigint]> = [],
  renames: Array<[string, string]> = [],
): Promise<string> {
  const backend = new MemoryProfileBackend();
  const store = new ProfileStore(backend, (a, b) => realWasm.profileIsNewer(a, b));
  for (const [key, name, stamp] of profiles) {
    const sniff = sniffProfile(realWasm, realWasm.composeProfileEnvelope(name, "vg3", stamp));
    if (sniff.kind !== "profile") throw new Error("not a profile");
    await store.ingest("acct", key, sniff.profile);
  }
  store.resave("acct");
  await new Promise((r) => setTimeout(r, 0));
  const row = backend.rows.get("acct") ?? { profiles: new Map(), verifications: new Map() };
  return encodeProfileRow(row, new Map(renames));
}

describe("a profile and a message in one wake name the sender", () => {
  it("[profile(VG3), chat(VG3)] reads 'VG3: text', with nothing stored yet", async () => {
    const { out, errors } = await run([profileRow(41, "VG3", 5n), chatRow(42, "hello")], await appRow());
    expect(out).toEqual({ title: "Test VG's", body: "VG3: hello", tag: GROUP_HEX });
    expect(errors).toBe(0);
  });

  it("[profile, reaction] reads 'VG3 reacted'", async () => {
    const { out } = await run([profileRow(41, "VG3", 5n), reactionRow(42)], await appRow());
    expect(out?.body).toBe("VG3 reacted ❤ to your message");
  });

  it("with no profile row at all on this device", async () => {
    const { out } = await run([profileRow(41, "VG3", 5n), chatRow(42, "hello")], null);
    expect(out?.body).toBe("VG3: hello");
  });

  it("a one-to-one: titled with the person", async () => {
    const { out } = await run(
      [profileRow(41, "VG3", 5n), chatRow(42, "hello")],
      await appRow(),
      wasmStub({ oneToOne: true }),
    );
    expect(out).toEqual({ title: "VG3", body: "hello", tag: GROUP_HEX });
  });

  it("a leaf with no account binding is named under its leaf key", async () => {
    const { out } = await run(
      [profileRow(41, "VG3", 5n), chatRow(42, "hello")],
      await appRow(),
      wasmStub({ bound: false }),
    );
    expect(out?.body).toBe("VG3: hello");
  });

  it("a newer profile in the batch replaces the stored one; an older one does not", async () => {
    const stored = await appRow([[b64(ACCOUNT), "VG3 old", 5n]]);
    expect((await run([profileRow(41, "VG3 new", 9n), chatRow(42, "hi")], stored)).out?.body).toBe("VG3 new: hi");
    expect((await run([profileRow(41, "VG3 replay", 3n), chatRow(42, "hi")], stored)).out?.body).toBe("VG3 old: hi");
  });

  it("a local rename still wins over the batch profile", async () => {
    const stored = await appRow([], [[b64(ACCOUNT), "Mum"]]);
    expect((await run([profileRow(41, "VG3", 9n), chatRow(42, "hi")], stored)).out?.body).toBe("Mum: hi");
  });

  it("the newest of two profiles in one batch is the one used", async () => {
    const { out } = await run(
      [profileRow(40, "VG3 first", 5n), profileRow(41, "VG3 second", 6n), chatRow(42, "hi")],
      await appRow(),
    );
    expect(out?.body).toBe("VG3 second: hi");
  });

  it("a profile alone is still no notification", async () => {
    const { out } = await run([profileRow(41, "VG3", 5n)], await appRow());
    expect(out).toBeNull();
  });

  it("a profile from ANOTHER member names only that member", async () => {
    const stub = wasmStub();
    const other = new Uint8Array(32).fill(0x55);
    const load = stub.wasm.Group.load;
    stub.wasm.Group.load = () => {
      const g = load();
      return { ...g, members: () => [...g.members(), { leafIndex: 3, signaturePublicKey: other }] };
    };
    const { out, errors } = await run([profileRow(41, "Other", 5n, 3), chatRow(42, "hi", 2)], await appRow(), stub);
    // VG3 has no name on this device: the defect banner, never "Other".
    expect(out).toEqual({ title: "Lakko", body: "New message", tag: GROUP_HEX });
    expect(errors).toBe(1);
  });
});
