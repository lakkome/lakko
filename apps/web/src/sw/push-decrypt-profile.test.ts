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
import { decryptPushPreview, groupNameFromRow, messageNotificationText, senderNameFromProfileRow } from "../../public/sw-push-decrypt.js";
import { encodeProfileRow } from "../profile/profile-idb";
import { conversationLabelsOf } from "../profile/member-label";
import { resolveDisplay } from "../profile/resolver";

/** Placeholder words a notification must never carry, in the languages the
 *  catalog has shipped them in. */
const PLACEHOLDERS = ["Member", "Someone", "Участник", "Кто-то"];
const hasPlaceholder = (text: string | undefined): boolean =>
  PLACEHOLDERS.some((w) => (text ?? "").includes(w));

/** ADR-0101 section 8: a v6 profile envelope is signed by an account
 *  identity; this test's profiles are signed by one fixed identity. */
let profileSignerHeld: realWasm.Identity | undefined;
function profileSigner(): realWasm.Identity {
  return (profileSignerHeld ??= realWasm.Identity.fromSeed(new Uint8Array(64).fill(0x5a)));
}

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
      readWakeBinding: async () => ({ accountId: "acct" }),
      lookupRoute: async () => ({ accountId: "acct", groupIdB64: Buffer.from(GROUP_BYTES).toString("base64"), subIdHex: "dd".repeat(16) }),
      getDeviceKey: async () => ({}),
      getIdentitySeed: async () => new Uint8Array(64),
      loadSnapshot: async () => ({ snapshotBytes: new Uint8Array([1]), knownGroupIds: [] }),
      wasm: wasmStub(),
      dsFetchGroup: async () => ({ messages }),
      // A sender always has a name on this device (the mirror the app writes).
      lookupSenderName: async () => "Test Sender",
      ...forbidden,
      ...overrides,
    },
  };
}

const wake = { subHex: "dd".repeat(16), groupHex: GROUP_HEX, pointer: 42 };
const profile = (name = "Zed Unique Name") => realWasm.composeProfileEnvelope(profileSigner(), name, "zed", 5n);

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

/** The stub core, its group a group (ADR-0062 kind 2). */
function groupWasm() {
  const base = wasmStub();
  return {
    ...base,
    GroupKind: { OneToOne: 1, Group: 2 },
    Group: {
      load: () => ({ ...base.Group.load(), groupKind: () => 2 }),
    },
  };
}

/** The stub core, its group a one-to-one. */
function oneToOneWasm() {
  const base = wasmStub();
  return {
    ...base,
    GroupKind: { OneToOne: 1, Group: 2 },
    Group: {
      load: () => ({ ...base.Group.load(), groupKind: () => 1 }),
    },
  };
}

describe("the notification names the conversation (ADR-0048 UX §5)", () => {
  const lookupSenderName = vi.fn(async (_a: string, _k: unknown, key: string) =>
    key === SENDER_B64 ? "Alice Petrova" : null,
  );

  it("a group: titled with its name, the body reads Sender: text", async () => {
    const lookupGroupName = vi.fn(async () => "Family");
    const { deps: d } = deps([chat(42, "hi")], { lookupSenderName, lookupGroupName, wasm: groupWasm() });
    const out = await decryptPushPreview(wake, d);
    expect(out?.title).toBe("Family");
    expect(out?.body).toBe("Alice Petrova: hi");
    expect(lookupSenderName).toHaveBeenCalledWith("acct", {}, SENDER_B64, undefined);
    expect(lookupGroupName).toHaveBeenCalledWith("acct", {}, GROUP_HEX);
  });

  it("a group with no known name: titled Lakko, still prefixed", async () => {
    const { deps: d } = deps([chat(42, "hi")], { lookupSenderName, lookupGroupName: async () => null, wasm: groupWasm() });
    const out = await decryptPushPreview(wake, d);
    expect(out?.title).toBe("Lakko");
    expect(out?.body).toBe("Alice Petrova: hi");
  });

  it("a one-to-one: titled with the person, just the text", async () => {
    const lookupGroupName = vi.fn(async () => "never asked");
    const { deps: d } = deps([chat(42, "hi")], { lookupSenderName, lookupGroupName, wasm: oneToOneWasm() });
    const out = await decryptPushPreview(wake, d);
    expect(out?.title).toBe("Alice Petrova");
    expect(out?.body).toBe("hi");
    expect(lookupGroupName).not.toHaveBeenCalled();
  });

  it("a sender with no name on this device is a defect: one plain error line and the generic banner", async () => {
    const errors = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      for (const lookup of [
        async () => null,
        async () => "   ",
        async () => {
          throw new Error("idb");
        },
      ]) {
        errors.mockClear();
        const { deps: d } = deps([chat(42, "hi")], {
          lookupSenderName: lookup,
          lookupGroupName: async () => "Family",
          wasm: groupWasm(),
        });
        const out = await decryptPushPreview(wake, d);
        // Neither a made-up name nor a body without its sender.
        expect(out).toEqual({ title: "Lakko", body: "New message", tag: GROUP_HEX });
        expect(errors).toHaveBeenCalledTimes(1);
        const line = String(errors.mock.calls[0]?.[0]);
        expect(line).not.toContain(SENDER_B64);
        expect(line).not.toContain(GROUP_HEX);
        expect(line).not.toContain("acct");
      }
    } finally {
      errors.mockRestore();
    }
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

describe("messageNotificationText — one shape on every client", () => {
  const words = { senderPrefixed: "\u0001: \u0002", reactedToYourMessage: "\u0001 reacted \u0002 to your message" };
  it("a group's media preview is prefixed like text", () => {
    expect(
      messageNotificationText({
        oneToOne: false,
        groupName: "Family",
        senderName: "Anna",
        text: "Sent an attachment",
        reactionEmoji: null,
        words,
      }),
    ).toEqual({ title: "Family", body: "Anna: Sent an attachment" });
  });
  it("a reaction keeps its own body, under the conversation's title", () => {
    expect(
      messageNotificationText({ oneToOne: false, groupName: "Family", senderName: "Anna", text: "x", reactionEmoji: "❤️", words }),
    ).toEqual({ title: "Family", body: "Anna reacted ❤️ to your message" });
    expect(
      messageNotificationText({ oneToOne: true, groupName: null, senderName: "Anna", text: "x", reactionEmoji: "❤️", words }),
    ).toEqual({ title: "Anna", body: "Anna reacted ❤️ to your message" });
  });
  it("a group whose kind could not be read is titled with the sender, who is not named twice", () => {
    expect(
      messageNotificationText({ oneToOne: null, groupName: null, senderName: "Anna", text: "hi", reactionEmoji: null, words }),
    ).toEqual({ title: "Anna", body: "hi" });
    expect(
      messageNotificationText({ oneToOne: null, groupName: "Family", senderName: "Anna", text: "hi", reactionEmoji: null, words }),
    ).toEqual({ title: "Family", body: "Anna: hi" });
  });
  it("no name, a blank name: there is no text to show (the caller shows the generic banner)", () => {
    for (const senderName of [null, "", "  "]) {
      for (const oneToOne of [true, false, null]) {
        for (const reactionEmoji of [null, "👍"]) {
          expect(
            messageNotificationText({ oneToOne, groupName: "Test VG's", senderName, text: "hi", reactionEmoji, words }),
          ).toBeNull();
        }
      }
    }
  });
});

describe("groupNameFromRow — the stored group-name row", () => {
  const b64 = Buffer.from(GROUP_BYTES).toString("base64");
  it("reads the group's name", () => {
    expect(groupNameFromRow({ v: 1, g: { [b64]: { n: "Family", t: "5" } } }, GROUP_HEX)).toBe("Family");
  });
  it("is null for another group, another schema or a bad id", () => {
    expect(groupNameFromRow({ v: 1, g: {} }, GROUP_HEX)).toBeNull();
    expect(groupNameFromRow({ v: 2, g: { [b64]: { n: "Family" } } }, GROUP_HEX)).toBeNull();
    expect(groupNameFromRow({ v: 1, g: { [b64]: { n: "Family" } } }, "zz")).toBeNull();
    expect(groupNameFromRow(null, GROUP_HEX)).toBeNull();
  });
});

describe("a reaction to your own message (Reactions section 12)", () => {
  const OWN_KEY = new Uint8Array(32).fill(0x0b);
  /** The stub core, this device at leaf 1, a one-to-one or a group. */
  function reactionWasm(oneToOne: boolean) {
    const base = wasmStub();
    return {
      ...base,
      GroupKind: { OneToOne: 1, Group: 2 },
      Group: {
        load: () => ({
          ...base.Group.load(),
          ownLeafIndex: () => 1,
          members: () => [
            { leafIndex: 1, signaturePublicKey: OWN_KEY },
            { leafIndex: 2, signaturePublicKey: SENDER_KEY },
          ],
          groupKind: () => (oneToOne ? 1 : 2),
        }),
      },
    };
  }
  const reaction = (id: number) => ({
    id,
    payload: new TextEncoder().encode(
      JSON.stringify({ kind: "reaction", emoji: "❤", reactionTargetAuthor: null, target: 7 }),
    ),
    createdAt: new Date(id * 1000).toISOString(),
  });
  function withTargetAuthor(oneToOne: boolean) {
    const w = reactionWasm(oneToOne);
    const parse = w.parseBodyEnvelope;
    return {
      ...w,
      parseBodyEnvelope: (bytes: Uint8Array) => {
        const p = parse(bytes) as { kind: string; reactionTargetAuthor?: unknown };
        return p.kind === "reaction" ? { ...p, reactionTargetAuthor: OWN_KEY } : p;
      },
    };
  }
  const lookupSenderName = async (_a: string, _k: unknown, key: string) =>
    key === SENDER_B64 ? "Anna" : null;

  it("says who reacted with what, under the group's name", async () => {
    const { deps: d } = deps([reaction(42)], {
      lookupSenderName,
      lookupGroupName: async () => "Family",
      wasm: withTargetAuthor(false),
    });
    const out = await decryptPushPreview(wake, d);
    expect(out?.title).toBe("Family");
    expect(out?.body).toBe("Anna reacted ❤ to your message");
  });

  it("in a one-to-one, under the person's name", async () => {
    const { deps: d } = deps([reaction(42)], { lookupSenderName, wasm: withTargetAuthor(true) });
    const out = await decryptPushPreview(wake, d);
    expect(out?.title).toBe("Anna");
    expect(out?.body).toBe("Anna reacted ❤ to your message");
  });
});

describe("the sender is looked up by the person key (ADR-0050)", () => {
  it("a member on a bound device is named from the profile filed under their account key", async () => {
    const ACCOUNT_KEY = new Uint8Array(32).fill(0x6a);
    const ACCOUNT_B64 = btoa(String.fromCharCode(...ACCOUNT_KEY));
    const base = wasmStub();
    const wasm = {
      ...base,
      Group: {
        load: () => ({
          ...base.Group.load(),
          members: () => [{ leafIndex: 2, signaturePublicKey: SENDER_KEY, accountKey: ACCOUNT_KEY }],
        }),
      },
    };
    const lookupSenderName = vi.fn(async (_a: string, _k: unknown, key: string) =>
      key === ACCOUNT_B64 ? "Anna" : null,
    );
    const { deps: d } = deps([chat(42, "hi")], { lookupSenderName, lookupGroupName: async () => "Family", wasm });
    const out = await decryptPushPreview(wake, d);
    expect(out?.body).toBe("Anna: hi");
    expect(lookupSenderName).toHaveBeenCalledWith("acct", {}, ACCOUNT_B64, undefined);
  });
});

describe("the worker finds a group member who is not a contact by every key it can be handed", () => {
  // The row built the way the running app builds it: the label mirror from
  // the rosters through the app's own rule (resolver -> memberNameOf), the
  // whole row through `encodeProfileRow`, read back as `sw.js` reads it
  // (JSON, then `senderNameFromProfileRow`).
  const LEAF = new Uint8Array(32).fill(0x31);
  const ACCOUNT = new Uint8Array(32).fill(0x32);
  const b64 = (b: Uint8Array): string => btoa(String.fromCharCode(...b));
  const roster = [{ signaturePublicKey: LEAF, accountKey: ACCOUNT }];
  function rowWith(profiles: Map<string, { displayName: string | null; username: string | null }>): unknown {
    const labels = conversationLabelsOf(
      [roster],
      (person) => {
        const p = profiles.get(person);
        const resolved = resolveDisplay({
          signatureKey: Uint8Array.from(atob(person), (c) => c.charCodeAt(0)),
          profile: p === undefined ? undefined : p,
        });
        // ProfileContext's rule: a person the app cannot name has no label.
        return resolved.source === "fingerprint" ? null : resolved.name;
      },
      b64,
    );
    const stored = new Map(
      [...profiles].map(([k, p]) => [
        k,
        { ...p, updatedAtMs: 1n, accountIdHex: null, usernameCertificateB64: null },
      ]),
    );
    return JSON.parse(encodeProfileRow({ profiles: stored, verifications: new Map() } as never, new Map(), labels));
  }

  it("a member with a received profile: by the account key and by the leaf key", () => {
    const row = rowWith(new Map([[b64(ACCOUNT), { displayName: "Vera G", username: null }]]));
    expect(senderNameFromProfileRow(row, b64(ACCOUNT))).toBe("Vera G");
    expect(senderNameFromProfileRow(row, b64(LEAF))).toBe("Vera G");
  });

  it("a member the app cannot name is left out of the mirror: no placeholder under either key", () => {
    const row = rowWith(new Map());
    expect(senderNameFromProfileRow(row, b64(ACCOUNT))).toBeNull();
    expect(senderNameFromProfileRow(row, b64(LEAF))).toBeNull();
    expect(JSON.stringify(row)).not.toMatch(/Member|Someone/);
  });

  it("end to end: a reaction from that member names them", async () => {
    const row = rowWith(new Map([[b64(ACCOUNT), { displayName: "Vera G", username: null }]]));
    const base = wasmStub();
    const OWN = new Uint8Array(32).fill(0x0b);
    const wasm = {
      ...base,
      GroupKind: { OneToOne: 1, Group: 2 },
      Group: {
        load: () => ({
          ...base.Group.load(),
          ownLeafIndex: () => 1,
          members: () => [
            { leafIndex: 1, signaturePublicKey: OWN },
            { leafIndex: 2, signaturePublicKey: LEAF, accountKey: ACCOUNT },
          ],
          groupKind: () => 2,
        }),
      },
      parseBodyEnvelope: (bytes: Uint8Array) => {
        const p = base.parseBodyEnvelope(bytes) as { kind: string };
        return p.kind === "reaction" ? { ...p, reactionTargetAuthor: OWN } : p;
      },
    };
    const reaction = {
      id: 42,
      payload: new TextEncoder().encode(JSON.stringify({ kind: "reaction", emoji: "\u2764", target: 7 })),
      createdAt: new Date(42_000).toISOString(),
    };
    const { deps: d } = deps([reaction], {
      wasm,
      // What sw.js does with the decrypted row.
      lookupSenderName: async (_a: string, _k: unknown, key: string) => senderNameFromProfileRow(row, key),
      lookupGroupName: async () => "Test VG's",
    });
    const out = await decryptPushPreview(wake, d);
    expect(out?.title).toBe("Test VG's");
    expect(out?.body).toBe("Vera G reacted \u2764 to your message");
  });

  it("end to end: a message from that member is prefixed with their name, looked up under the leaf key too", async () => {
    const row = rowWith(new Map([[b64(ACCOUNT), { displayName: "Vera G", username: null }]]));
    const base = wasmStub();
    // A leaf with no account binding read: the worker asks by the leaf key.
    const wasm = {
      ...base,
      GroupKind: { OneToOne: 1, Group: 2 },
      Group: {
        load: () => ({
          ...base.Group.load(),
          members: () => [{ leafIndex: 2, signaturePublicKey: LEAF }],
          groupKind: () => 2,
        }),
      },
    };
    const { deps: d } = deps([chat(42, "hi")], {
      wasm,
      lookupSenderName: async (_a: string, _k: unknown, key: string) => senderNameFromProfileRow(row, key),
      lookupGroupName: async () => "Test VG's",
    });
    const out = await decryptPushPreview(wake, d);
    expect(out).toEqual({ title: "Test VG's", body: "Vera G: hi", tag: GROUP_HEX });
  });
});

describe("conversationLabelsOf — the mirror holds real names only", () => {
  const b64 = (b: Uint8Array): string => btoa(String.fromCharCode(...b));
  const LEAF = new Uint8Array(32).fill(0x41);
  const ACCOUNT = new Uint8Array(32).fill(0x42);
  const CONTACT = b64(new Uint8Array(32).fill(0x43));
  const STRANGER = b64(new Uint8Array(32).fill(0x44));
  const names = new Map([
    [b64(ACCOUNT), "Vera G"],
    [CONTACT, "Oleg"],
  ]);

  it("adds contacts and other keys, leaves out anyone without a name or whose lookup throws", () => {
    const labels = conversationLabelsOf(
      [[{ signaturePublicKey: LEAF, accountKey: ACCOUNT }], [{ signaturePublicKey: new Uint8Array(32).fill(0x45) }]],
      (key) => {
        if (key === STRANGER) throw new Error("boom");
        return names.get(key) ?? null;
      },
      b64,
      [CONTACT, STRANGER, b64(ACCOUNT)],
    );
    expect(Object.fromEntries(labels)).toEqual({
      [b64(ACCOUNT)]: "Vera G",
      [b64(LEAF)]: "Vera G",
      [CONTACT]: "Oleg",
    });
  });

  it("a blank name is no name", () => {
    expect(conversationLabelsOf([[{ signaturePublicKey: LEAF }]], () => "   ", b64).size).toBe(0);
  });
});

describe("no notification text ever carries a placeholder word", () => {
  it("over every shape the worker builds, named or not", async () => {
    const errors = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      const outs: unknown[] = [];
      for (const wasm of [wasmStub(), groupWasm(), oneToOneWasm()]) {
        for (const lookupSenderName of [async () => "Anna", async () => null]) {
          for (const lookupGroupName of [async () => "Family", async () => null]) {
            const { deps: d } = deps([chat(42, "hi")], { wasm, lookupSenderName, lookupGroupName });
            outs.push(await decryptPushPreview(wake, d));
          }
        }
      }
      expect(outs.length).toBe(12);
      for (const out of outs as Array<{ title?: string; body?: string } | null>) {
        expect(out).not.toBeNull();
        expect(hasPlaceholder(out?.title)).toBe(false);
        expect(hasPlaceholder(out?.body)).toBe(false);
      }
    } finally {
      errors.mockRestore();
    }
  });
});
