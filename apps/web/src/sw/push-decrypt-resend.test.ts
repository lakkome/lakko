// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.7 case 1 — the service worker knows the RE-SEND MARKER: a push
 * for a re-sent message whose original this device already holds posts
 * nothing.
 *
 * Two layers, as in `push-decrypt-mute.test.ts`:
 *
 *   - `resendHeldFromRows` is the worker's hand-rolled copy of
 *     `src/chat/resend-marker.ts` `resendVerdict` over two rows the APP
 *     writes. It is driven with the real wasm fold and the app's own
 *     encoder, and cross-checked against the app's rule, so the two cannot
 *     drift apart silently;
 *   - `decryptPushPreview` is driven with mock deps to pin what a held
 *     re-send does to the notification, and that nothing is written (C-59).
 *
 * Not covered: the IndexedDB / WebCrypto plumbing in `public/sw.js`, which
 * vitest's bare node environment does not have.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it, vi } from "vitest";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { encodeSyncState } from "../devices/sync/sync-state-store";
import { heldRowAuthor, resendVerdict } from "../chat/resend-marker";
// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { decryptPushPreview, resendHeldFromRows } from "../../public/sw-push-decrypt.js";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

function hexBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}
function b64(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

const ROOT_HEX = "ab".repeat(32); // the conversation's first group
const SECOND_HEX = "cd".repeat(32); // its first successor
const THIRD_HEX = "ef".repeat(32); // a later successor
const ELSEWHERE_HEX = "12".repeat(32); // another conversation
const OWN = "b3duLWFjY291bnQta2V5";
const PEER = "cGVlci1hY2NvdW50LWtleQ==";

/** The app's sync row: ROOT continued in SECOND, then in THIRD. */
function syncRow(): unknown {
  const s = new wasm.SyncedState();
  expect(s.setLineage(9n, hexBytes(SECOND_HEX), hexBytes(ROOT_HEX))).toBe(true);
  expect(s.setLineage(12n, hexBytes(THIRD_HEX), hexBytes(ROOT_HEX))).toBe(true);
  return JSON.parse(encodeSyncState({ synced: s.toBytes(), pending: [] }));
}

type Row = { messageId: string; direction: "outgoing" | "incoming" | "system"; authorKeyB64?: string };

/** The app's message log, reduced to what the rule reads. */
function log(groups: Record<string, Row[]>): unknown {
  return {
    version: 1,
    groups: Object.fromEntries(
      Object.entries(groups).map(([hex, messages]) => [hex, { cursor: "0", messages }]),
    ),
  };
}

function question(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    receivedGroupHex: SECOND_HEX,
    originGroupB64: b64(hexBytes(ROOT_HEX)),
    originMessageId: 7,
    senderAccountKeyB64: OWN,
    ownAccountKeyB64: OWN,
    ...over,
  };
}

const held = (l: unknown, q: Record<string, unknown>): boolean =>
  resendHeldFromRows(l, syncRow(), q, wasm) as boolean;

describe("resendHeldFromRows: the worker's copy of the re-send rule", () => {
  it("a row this device sent itself, re-sent by its own account into the successor, is held", () => {
    expect(held(log({ [ROOT_HEX]: [{ messageId: "7", direction: "outgoing" }] }), question())).toBe(true);
  });

  it("a row another device of the account sent (stored as incoming under the account key) is held", () => {
    expect(
      held(log({ [ROOT_HEX]: [{ messageId: "7", direction: "incoming", authorKeyB64: OWN }] }), question()),
    ).toBe(true);
  });

  it("an original in an earlier SUCCESSOR is held when the message arrives in a later one", () => {
    expect(
      held(
        log({ [SECOND_HEX]: [{ messageId: "7", direction: "outgoing" }] }),
        question({ receivedGroupHex: THIRD_HEX, originGroupB64: b64(hexBytes(SECOND_HEX)) }),
      ),
    ).toBe(true);
  });

  it("the row is not held: the message is previewed", () => {
    expect(held(log({ [ROOT_HEX]: [{ messageId: "8", direction: "outgoing" }] }), question())).toBe(false);
    expect(held(log({}), question())).toBe(false);
  });

  it("a marker naming somebody else's row is ignored", () => {
    expect(
      held(log({ [ROOT_HEX]: [{ messageId: "7", direction: "incoming", authorKeyB64: PEER }] }), question()),
    ).toBe(false);
    // A peer claiming this account's row.
    expect(
      held(
        log({ [ROOT_HEX]: [{ messageId: "7", direction: "outgoing" }] }),
        question({ senderAccountKeyB64: PEER }),
      ),
    ).toBe(false);
  });

  it("a system row has no author", () => {
    expect(held(log({ [ROOT_HEX]: [{ messageId: "7", direction: "system" }] }), question())).toBe(false);
  });

  it("the origin must be an OLDER segment of the SAME conversation", () => {
    const sent: Row[] = [{ messageId: "7", direction: "outgoing" }];
    // The same group.
    expect(held(log({ [SECOND_HEX]: sent }), question({ originGroupB64: b64(hexBytes(SECOND_HEX)) }))).toBe(false);
    // A LATER segment.
    expect(held(log({ [THIRD_HEX]: sent }), question({ originGroupB64: b64(hexBytes(THIRD_HEX)) }))).toBe(false);
    // Another conversation.
    expect(
      held(log({ [ELSEWHERE_HEX]: sent }), question({ originGroupB64: b64(hexBytes(ELSEWHERE_HEX)) })),
    ).toBe(false);
    // Received in a group that is nobody's successor.
    expect(held(log({ [ROOT_HEX]: sent }), question({ receivedGroupHex: ELSEWHERE_HEX }))).toBe(false);
  });

  it("an unattributable sender never matches", () => {
    const l = log({ [ROOT_HEX]: [{ messageId: "7", direction: "outgoing" }] });
    expect(held(l, question({ senderAccountKeyB64: null }))).toBe(false);
    expect(held(l, question({ ownAccountKeyB64: null }))).toBe(false);
  });

  it("anything unreadable is not held", () => {
    const l = log({ [ROOT_HEX]: [{ messageId: "7", direction: "outgoing" }] });
    for (const bad of [null, { v: 1 }, { v: 2, s: "" }, { v: 2, s: btoa("not a fold") }]) {
      expect(resendHeldFromRows(l, bad, question(), wasm)).toBe(false);
    }
    expect(resendHeldFromRows(null, syncRow(), question(), wasm)).toBe(false);
    expect(resendHeldFromRows(l, syncRow(), question({ originGroupB64: "!!" }), wasm)).toBe(false);
    expect(resendHeldFromRows(l, syncRow(), question(), null)).toBe(false);
    expect(resendHeldFromRows(l, syncRow(), null, wasm)).toBe(false);
  });

  it("agrees with the app's rule on every case of its table", () => {
    const segments = [ROOT_HEX, SECOND_HEX, THIRD_HEX];
    const rows: (Row | undefined)[] = [
      undefined,
      { messageId: "7", direction: "outgoing" },
      { messageId: "7", direction: "incoming", authorKeyB64: OWN },
      { messageId: "7", direction: "incoming", authorKeyB64: PEER },
      { messageId: "7", direction: "system" },
    ];
    for (const received of [...segments, ELSEWHERE_HEX]) {
      for (const origin of [...segments, ELSEWHERE_HEX]) {
        for (const row of rows) {
          for (const sender of [OWN, PEER]) {
            const conversationOf = (hex: string): string => (segments.includes(hex) ? ROOT_HEX : hex);
            const app =
              resendVerdict({
                marker: { groupIdB64: b64(hexBytes(origin)), messageId: 7 },
                receivedConversation: conversationOf(received),
                originConversation: row === undefined ? null : conversationOf(origin),
                originIsOlderSegment: segments.indexOf(origin) < segments.indexOf(received),
                heldAuthor: heldRowAuthor(row, OWN, (k) => k),
                senderAuthor: sender,
              }) === "skip";
            const worker = held(
              log(row === undefined ? {} : { [origin]: [row] }),
              question({
                receivedGroupHex: received,
                originGroupB64: b64(hexBytes(origin)),
                senderAccountKeyB64: sender,
              }),
            );
            expect(worker, `${received.slice(0, 2)} <- ${origin.slice(0, 2)} ${row?.direction} ${sender}`).toBe(app);
          }
        }
      }
    }
  });
});

// ---------------------------------------------------------------------------
// decryptPushPreview
// ---------------------------------------------------------------------------

const ACCOUNT_ID = "YWNjb3VudC1pZA==";
const GROUP_BYTES = hexBytes(SECOND_HEX);
const ORIGIN_B64 = b64(hexBytes(ROOT_HEX));

function msg(id: number, body: Record<string, unknown>) {
  return { id, payload: new TextEncoder().encode(JSON.stringify(body)), createdAt: "2026-10-12T00:00:00Z" };
}

const resent = (id: number, text: string) =>
  msg(id, { kind: "chat", text, attachmentCount: 0, resendOriginGroupB64: ORIGIN_B64, resendOriginMessageId: 7 });

/**
 * `senderAccount` is the account byte of the sender's leaf: a peer (0xbb) by
 * default, since a message from this device's own account (0xaa) is never a
 * notification (`push-decrypt-own-wake.test.ts`).
 */
function deps(
  messages: ReturnType<typeof msg>[],
  overrides: Record<string, unknown> = {},
  senderAccount = 0xbb,
) {
  const forbidden = { saveSnapshot: vi.fn(), saveMessageLog: vi.fn(), advanceCursor: vi.fn() };
  const members = [
    { leafIndex: 0, signaturePublicKey: new Uint8Array(32).fill(1), accountKey: new Uint8Array(32).fill(0xaa) },
    { leafIndex: 1, signaturePublicKey: new Uint8Array(32).fill(2), accountKey: new Uint8Array(32).fill(senderAccount) },
  ];
  return {
    forbidden,
    deps: {
      readWakeBinding: async () => ({ accountId: ACCOUNT_ID }),
      // A sender always has a name on this device (the mirror the app writes).
      lookupSenderName: async () => "Test Sender",
      lookupRoute: async () => ({ accountId: ACCOUNT_ID, groupIdB64: Buffer.from(GROUP_BYTES).toString("base64"), subIdHex: "dd".repeat(16) }),
      getDeviceKey: async () => ({}),
      getIdentitySeed: async () => new Uint8Array(64),
      loadSnapshot: async () => ({ snapshotBytes: new Uint8Array([1]), knownGroupIds: [] }),
      wasm: {
        Identity: { fromSeed: () => ({ restoreStorage: () => undefined, free: () => undefined }) },
        Group: {
          load: () => ({
            ownLeafIndex: 0,
            members: () => members,
            processIncomingVerifyingRoles: (_i: unknown, payload: Uint8Array) => ({
              kind: "Application",
              applicationData: payload,
              senderLeaf: 1,
            }),
            free: () => undefined,
          }),
        },
        parseBodyEnvelope: (bytes: Uint8Array) => ({
          ...(JSON.parse(new TextDecoder().decode(bytes)) as Record<string, unknown>),
          free: () => undefined,
        }),
      },
      dsFetchGroup: async () => ({ messages }),
      ...forbidden,
      ...overrides,
    },
  };
}

const wake = { subHex: "dd".repeat(16), groupHex: SECOND_HEX, pointer: 42 };

describe("decryptPushPreview and a re-sent message", () => {
  it("a re-send whose original this device holds posts nothing", async () => {
    const lookupResendHeld = vi.fn(async () => true);
    const d = deps([resent(42, "said before")], { lookupResendHeld }, 0xaa);
    await expect(decryptPushPreview(wake, d.deps)).resolves.toBeNull();
    // Asked about exactly this message, with the PERSON keys of the sender
    // and of this account (two leaves of one account here).
    const account = b64(new Uint8Array(32).fill(0xaa));
    expect(lookupResendHeld).toHaveBeenCalledWith(ACCOUNT_ID, expect.anything(), {
      receivedGroupHex: SECOND_HEX,
      originGroupB64: ORIGIN_B64,
      originMessageId: 7,
      senderAccountKeyB64: account,
      ownAccountKeyB64: account,
    });
    for (const spy of Object.values(d.forbidden)) expect(spy).not.toHaveBeenCalled();
  });

  it("a re-send this device does NOT hold is a message like any other", async () => {
    const d = deps([resent(42, "new to this device")], { lookupResendHeld: async () => false });
    await expect(decryptPushPreview(wake, d.deps)).resolves.toMatchObject({ body: "new to this device" });
  });

  it("a held re-send leaves an older real message of the batch as the preview", async () => {
    const d = deps(
      [msg(41, { kind: "chat", text: "really new", attachmentCount: 0 }), resent(42, "said before")],
      { lookupResendHeld: async () => true },
    );
    await expect(decryptPushPreview(wake, d.deps)).resolves.toMatchObject({ body: "really new" });
  });

  it("a message with no marker never asks", async () => {
    const lookupResendHeld = vi.fn(async () => true);
    const d = deps([msg(42, { kind: "chat", text: "plain", attachmentCount: 0 })], { lookupResendHeld });
    await expect(decryptPushPreview(wake, d.deps)).resolves.toMatchObject({ body: "plain" });
    expect(lookupResendHeld).not.toHaveBeenCalled();
  });

  it("half a marker is no marker", async () => {
    const lookupResendHeld = vi.fn(async () => true);
    const d = deps(
      [msg(42, { kind: "chat", text: "half", attachmentCount: 0, resendOriginGroupB64: ORIGIN_B64 })],
      { lookupResendHeld },
    );
    await expect(decryptPushPreview(wake, d.deps)).resolves.toMatchObject({ body: "half" });
    expect(lookupResendHeld).not.toHaveBeenCalled();
  });

  it("a lookup that throws, or a worker with no lookup, previews the message", async () => {
    const thrown = deps([resent(42, "kept")], {
      lookupResendHeld: async () => {
        throw new Error("idb did not open");
      },
    });
    await expect(decryptPushPreview(wake, thrown.deps)).resolves.toMatchObject({ body: "kept" });
    await expect(decryptPushPreview(wake, deps([resent(42, "kept")]).deps)).resolves.toMatchObject({
      body: "kept",
    });
  });
});
