// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A wake shows a notification only for a message from someone else (or a
 * call).
 *
 * The DS wakes every subscription of a group, the sender's own among them, so
 * each message comes back to the device that sent it. That device cannot open
 * its own message, and before this rule the worker showed the generic
 * "New message" notification for it. These tests pin what `decryptPushWake`
 * reports and what `genericNotificationWanted` decides from it — the same
 * cases as Android's `OwnWakeTest`.
 *
 * Not covered: the IndexedDB / WebCrypto plumbing in `public/sw.js` (the
 * message-log read behind `lookupHeldMessageIds`, the visible-tab branch),
 * which vitest's bare node environment does not have.
 */
import { describe, expect, it, vi } from "vitest";
// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { decryptPushPreview, decryptPushWake, genericNotificationWanted, heldMessageIdsFromLogRow, isOwnMessageEcho } from "../../public/sw-push-decrypt.js";

const GROUP_HEX = "ab".repeat(32);
const GROUP_BYTES = new Uint8Array(32).fill(0xab);
const ACCOUNT_ID = "YWNjb3VudC1pZA==";
const SUB_HEX = "dd".repeat(16);

/** Leaves: 0 this device, 1 another device of the same account, 2 a peer. */
const OWN_LEAF = 0;
const OWN_OTHER_DEVICE_LEAF = 1;
const PEER_LEAF = 2;
const MEMBERS = [
  { leafIndex: OWN_LEAF, signaturePublicKey: new Uint8Array(32).fill(1), accountKey: new Uint8Array(32).fill(0xaa) },
  {
    leafIndex: OWN_OTHER_DEVICE_LEAF,
    signaturePublicKey: new Uint8Array(32).fill(2),
    accountKey: new Uint8Array(32).fill(0xaa),
  },
  { leafIndex: PEER_LEAF, signaturePublicKey: new Uint8Array(32).fill(3), accountKey: new Uint8Array(32).fill(0xbb) },
];

type Row = { id: number; payload: Uint8Array; createdAt: string };

/** A row that decrypts to `body`, sent from `leaf`. */
function decrypts(id: number, leaf: number, body: Record<string, unknown>, kind = "Application"): Row {
  return {
    id,
    payload: new TextEncoder().encode(JSON.stringify({ leaf, kind, body })),
    createdAt: new Date(id * 1000).toISOString(),
  };
}

/** A row the throwaway group cannot open, refused without naming why (as a
 *  wrong epoch is): the message log decides whether it is this device's own. */
function undecryptable(id: number): Row {
  return { id, payload: new TextEncoder().encode("THROW"), createdAt: new Date(id * 1000).toISOString() };
}

/** A row the core refuses as this device's own message (its typing or
 *  receipt, which the message log never holds). */
function ownEcho(id: number): Row {
  return { id, payload: new TextEncoder().encode("OWN"), createdAt: new Date(id * 1000).toISOString() };
}

const chat = (text: string) => ({ kind: "chat", text, attachmentCount: 0 });

function deps(rows: Row[], overrides: Record<string, unknown> = {}) {
  const forbidden = { saveSnapshot: vi.fn(), saveMessageLog: vi.fn(), advanceCursor: vi.fn() };
  return {
    forbidden,
    deps: {
      readWakeBinding: async () => ({ accountId: ACCOUNT_ID }),
      // A sender always has a name on this device (the mirror the app writes).
      lookupSenderName: async () => "Test Sender",
      lookupRoute: async () => ({
        accountId: ACCOUNT_ID,
        groupIdB64: Buffer.from(GROUP_BYTES).toString("base64"),
        subIdHex: SUB_HEX,
      }),
      getDeviceKey: async () => ({}),
      getIdentitySeed: async () => new Uint8Array(64),
      loadSnapshot: async () => ({ snapshotBytes: new Uint8Array([1]), knownGroupIds: [] }),
      wasm: {
        Identity: { fromSeed: () => ({ restoreStorage: () => undefined, free: () => undefined }) },
        Group: {
          load: () => ({
            ownLeafIndex: OWN_LEAF,
            members: () => MEMBERS,
            processIncomingVerifyingRoles: (_i: unknown, payload: Uint8Array) => {
              const text = new TextDecoder().decode(payload);
              if (text === "THROW") throw new Error("process_message: WrongEpoch");
              if (text === "OWN") throw new Error("process_message: ValidationError(CannotDecryptOwnMessage)");
              const row = JSON.parse(text) as { leaf: number; kind: string; body: unknown };
              return {
                kind: row.kind,
                applicationData: new TextEncoder().encode(JSON.stringify(row.body)),
                senderLeaf: row.leaf,
              };
            },
            free: () => undefined,
          }),
        },
        parseBodyEnvelope: (bytes: Uint8Array) => ({
          ...(JSON.parse(new TextDecoder().decode(bytes)) as Record<string, unknown>),
          free: () => undefined,
        }),
      },
      dsFetchGroup: async () => ({ messages: rows }),
      ...forbidden,
      ...overrides,
    },
  };
}

const wake = { subHex: SUB_HEX, groupHex: GROUP_HEX, pointer: 20 };

describe("decryptPushWake: a wake that brings nothing from someone else shows nothing", () => {
  it("this device's own message coming back shows nothing", async () => {
    // The row does not decrypt (a sender cannot open its own message) and the
    // app's log holds it: it is ours.
    const lookupHeldMessageIds = vi.fn(async () => new Set([20]));
    const d = deps([undecryptable(20)], { lookupHeldMessageIds });
    const outcome = await decryptPushWake(wake, d.deps);

    expect(outcome).toEqual({ preview: null, readRan: true, unreadableRows: 0 });
    expect(genericNotificationWanted(outcome)).toBe(false);
    expect(lookupHeldMessageIds).toHaveBeenCalledTimes(1);
    expect(lookupHeldMessageIds).toHaveBeenCalledWith(ACCOUNT_ID, expect.anything(), GROUP_HEX);
    for (const spy of Object.values(d.forbidden)) expect(spy).not.toHaveBeenCalled();
  });

  it("the log is read once per wake, however many rows do not decrypt", async () => {
    const lookupHeldMessageIds = vi.fn(async () => [18, 19, 20]);
    const d = deps([undecryptable(18), undecryptable(19), undecryptable(20)], { lookupHeldMessageIds });
    const outcome = await decryptPushWake(wake, d.deps);

    expect(outcome.unreadableRows).toBe(0);
    expect(genericNotificationWanted(outcome)).toBe(false);
    expect(lookupHeldMessageIds).toHaveBeenCalledTimes(1);
  });

  it("a message from another device of the same account shows nothing", async () => {
    const d = deps([decrypts(20, OWN_OTHER_DEVICE_LEAF, chat("sent from my laptop"))]);
    const outcome = await decryptPushWake(wake, d.deps);

    expect(outcome.preview).toBeNull();
    expect(outcome.readRan).toBe(true);
    expect(genericNotificationWanted(outcome)).toBe(false);
  });

  it("service traffic alone shows nothing", async () => {
    // A read receipt, typing, a reaction from a peer, and a commit: decrypted,
    // nothing to show.
    const d = deps([
      decrypts(17, PEER_LEAF, { kind: "read-receipt" }),
      decrypts(18, PEER_LEAF, { kind: "typing" }),
      decrypts(19, PEER_LEAF, { kind: "reaction" }),
      decrypts(20, PEER_LEAF, {}, "Commit"),
    ]);
    const outcome = await decryptPushWake(wake, d.deps);

    expect(outcome).toEqual({ preview: null, readRan: true, unreadableRows: 0 });
    expect(genericNotificationWanted(outcome)).toBe(false);
  });

  it("a message from someone else is the preview, even when a newer own row follows", async () => {
    const d = deps(
      [
        decrypts(18, PEER_LEAF, chat("hi")),
        decrypts(19, OWN_OTHER_DEVICE_LEAF, chat("mine, from my laptop")),
        undecryptable(20),
      ],
      { lookupHeldMessageIds: async () => new Set([20]) },
    );
    const outcome = await decryptPushWake(wake, d.deps);

    expect(outcome.preview).toMatchObject({ body: "hi", tag: GROUP_HEX });
    expect(genericNotificationWanted(outcome)).toBe(false);
    // The reduced form is the same preview.
    await expect(decryptPushPreview(wake, d.deps)).resolves.toMatchObject({ body: "hi" });
  });

  it("a message from someone else is the preview even next to a row this device cannot read", async () => {
    const d = deps([undecryptable(19), decrypts(20, PEER_LEAF, chat("hello"))]);
    const outcome = await decryptPushWake(wake, d.deps);

    expect(outcome.preview).toMatchObject({ body: "hello" });
    expect(outcome.unreadableRows).toBe(1);
    expect(genericNotificationWanted(outcome)).toBe(false);
  });
});

describe("decryptPushWake: what cannot be told apart keeps the generic notification", () => {
  it("a row that does not decrypt and is not held keeps the generic notification", async () => {
    const d = deps([undecryptable(20)], { lookupHeldMessageIds: async () => new Set([7]) });
    const outcome = await decryptPushWake(wake, d.deps);

    expect(outcome).toEqual({ preview: null, readRan: true, unreadableRows: 1 });
    expect(genericNotificationWanted(outcome)).toBe(true);
  });

  it("an unreadable row next to own rows still keeps the generic notification", async () => {
    const d = deps(
      [decrypts(18, OWN_OTHER_DEVICE_LEAF, chat("mine")), undecryptable(19), undecryptable(20)],
      { lookupHeldMessageIds: async () => new Set([20]) },
    );
    const outcome = await decryptPushWake(wake, d.deps);

    expect(outcome.preview).toBeNull();
    expect(outcome.unreadableRows).toBe(1);
    expect(genericNotificationWanted(outcome)).toBe(true);
  });

  it("no log lookup, a log lookup that answers null, and one that throws all read as nothing held", async () => {
    for (const overrides of [
      {},
      { lookupHeldMessageIds: async () => null },
      {
        lookupHeldMessageIds: async () => {
          throw new Error("idb did not open");
        },
      },
    ]) {
      const outcome = await decryptPushWake(wake, deps([undecryptable(20)], overrides).deps);
      expect(outcome.unreadableRows).toBe(1);
      expect(genericNotificationWanted(outcome)).toBe(true);
    }
  });

  it("a read that did not run keeps the generic notification", async () => {
    const cases: Record<string, unknown>[] = [
      { loadSnapshot: async () => null },
      { getIdentitySeed: async () => null },
      { getDeviceKey: async () => null },
      {
        dsFetchGroup: async () => {
          throw new Error("ds fetch 503");
        },
      },
    ];
    for (const overrides of cases) {
      const outcome = await decryptPushWake(wake, deps([undecryptable(20)], overrides).deps);
      expect(outcome).toEqual({ preview: null, readRan: false, unreadableRows: 0 });
      expect(genericNotificationWanted(outcome)).toBe(true);
    }
    // A wake the core cannot even start on.
    const none = await decryptPushWake(null, deps([]).deps);
    expect(none.readRan).toBe(false);
    expect(genericNotificationWanted(none)).toBe(true);
    // No outcome at all (the worker could not load the core).
    expect(genericNotificationWanted(null)).toBe(true);
  });
});

describe("heldMessageIdsFromLogRow", () => {
  it("returns the stored ids of the group as numbers and skips anything malformed", () => {
    const payload = {
      version: 1,
      groups: {
        [GROUP_HEX]: {
          cursor: "20",
          messages: [
            { messageId: "18", direction: "incoming" },
            { messageId: "20", direction: "outgoing" },
            { messageId: "not a number", direction: "outgoing" },
            { messageId: "-1", direction: "outgoing" },
            null,
          ],
        },
        ["cd".repeat(32)]: { cursor: "0", messages: [{ messageId: "99", direction: "outgoing" }] },
      },
    };
    expect([...heldMessageIdsFromLogRow(payload, GROUP_HEX)].sort()).toEqual([18, 20]);
  });

  it("anything unreadable is the empty set", () => {
    for (const bad of [null, undefined, {}, { groups: {} }, { groups: { [GROUP_HEX]: { messages: "x" } } }]) {
      expect(heldMessageIdsFromLogRow(bad, GROUP_HEX).size).toBe(0);
    }
  });
});

describe("the core's own-message refusal", () => {
  it("is quiet even when the log does not hold the row", async () => {
    const d = deps([ownEcho(19), ownEcho(20)]);
    const outcome = await decryptPushWake(wake, d.deps);
    expect(outcome.unreadableRows).toBe(0);
    expect(genericNotificationWanted(outcome)).toBe(false);
  });
});

describe("isOwnMessageEcho", () => {
  it("names the core's own-message refusal, nothing else", () => {
    expect(isOwnMessageEcho(new Error("process_message: ValidationError(CannotDecryptOwnMessage)"))).toBe(true);
    expect(isOwnMessageEcho("message: CannotDecryptOwnMessage")).toBe(true);
    expect(isOwnMessageEcho(new Error("process_message: WrongEpoch"))).toBe(false);
  });
});
