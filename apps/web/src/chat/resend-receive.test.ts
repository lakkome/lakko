// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.7 case 1 - the re-send marker end to end on this client: the
 * envelope the founder composes carries it, the parser reads it back, and
 * the receive predicate drops the successor row exactly when the original
 * is held (`resend-receive.ts`).
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import { groupIdHex } from "../devices/sync/state";
import { composeEnvelope, composeResendEnvelope, parseBodyEnvelope } from "./attachments";
import type { IncomingChatEvent } from "./client";
import { buildLineage } from "./lineage";
import { MESSAGE_LOG_VERSION, type MessageLogPayload, type PersistedMessage } from "./message-log-store";
import { heldResendPredicate, type ResendReceivePort } from "./resend-receive";
import { resendParts } from "./reset-repair";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const OLD = new Uint8Array(32).fill(0x01);
const NEW = new Uint8Array(32).fill(0x02);
const OTHER = new Uint8Array(32).fill(0x03);
const OLD_B64 = base64Encode(OLD);

describe("the marker on the wire", () => {
  it("a re-send carries the original group id and message id, and parses back", async () => {
    const bytes = await composeResendEnvelope("again", [], { groupIdB64: OLD_B64, messageId: 41 });
    const body = await parseBodyEnvelope(bytes);
    expect(body.kind).toBe("chat");
    if (body.kind !== "chat") return;
    expect(body.text).toBe("again");
    expect(body.resendOrigin).toEqual({ groupIdB64: OLD_B64, messageId: 41 });
  });
  it("a re-sent REPLY keeps its quote, with target 0 (no jump)", async () => {
    const parts = resendParts({ reply: { target: 17, author: "Ada", excerpt: "see you at nine" } });
    const bytes = await composeResendEnvelope("ok", [], { groupIdB64: OLD_B64, messageId: 41 }, parts);
    const body = await parseBodyEnvelope(bytes);
    if (body.kind !== "chat") throw new Error("not a chat body");
    expect(body.reply).toEqual({ target: 0, author: "Ada", excerpt: "see you at nine" });
    expect(body.forward).toBeUndefined();
    expect(body.resendOrigin).toEqual({ groupIdB64: OLD_B64, messageId: 41 });
  });
  it("a re-sent FORWARD keeps its attribution and its origin", async () => {
    const forward = { author: "Grace", origin: { groupIdB64: base64Encode(OTHER), messageId: 9 } };
    const bytes = await composeResendEnvelope(
      "fwd",
      [],
      { groupIdB64: OLD_B64, messageId: 42 },
      resendParts({ forward }),
    );
    const body = await parseBodyEnvelope(bytes);
    if (body.kind !== "chat") throw new Error("not a chat body");
    expect(body.forward).toEqual(forward);
    expect(body.reply).toBeUndefined();
    expect(body.resendOrigin).toEqual({ groupIdB64: OLD_B64, messageId: 42 });
  });
  it("an ordinary message carries none", async () => {
    const body = await parseBodyEnvelope(await composeEnvelope("plain", []));
    expect(body.kind === "chat" ? body.resendOrigin : "not-chat").toBeUndefined();
  });
});

type App = Extract<IncomingChatEvent, { kind: "application" }>;

function received(senderKey: string | undefined, marker?: { groupIdB64: string; messageId: number }): App {
  return {
    kind: "application",
    messageId: 3,
    text: "again",
    plaintext: "again",
    attachments: [],
    senderLeaf: 0,
    senderKey,
    createdAt: "2026-10-11T00:00:00.000Z",
    ...(marker === undefined ? {} : { resendOrigin: marker }),
  } as App;
}

function row(messageId: number, direction: PersistedMessage["direction"], authorKeyB64?: string): PersistedMessage {
  return {
    messageId: String(messageId),
    direction,
    text: "original",
    attachments: [],
    createdAt: "2026-10-10T00:00:00.000Z",
    ...(authorKeyB64 === undefined ? {} : { authorKeyB64 }),
  };
}

function port(
  log: MessageLogPayload | null,
  over: Partial<ResendReceivePort> = {},
): ResendReceivePort & { loads: number } {
  // OLD -> NEW is one conversation; OTHER is its own.
  const lineage = buildLineage([{ successorHex: groupIdHex(NEW), rootHex: groupIdHex(OLD), hlc: 10n }]);
  const p = {
    loads: 0,
    receivedGroupId: NEW,
    conversationHexOf: (id: Uint8Array) => lineage.conversationOf(groupIdHex(id)),
    segmentIndexOf: (id: Uint8Array) => lineage.segmentIndexOf(groupIdHex(id)),
    ownPersonKeyB64: "own",
    canonical: (k: string) => k,
    loadLog: async () => {
      p.loads += 1;
      return log;
    },
    ...over,
  };
  return p;
}

function logWith(groupId: Uint8Array, messages: PersistedMessage[]): MessageLogPayload {
  return {
    version: MESSAGE_LOG_VERSION,
    groups: { [groupIdHex(groupId)]: { cursor: "9", messages } },
  } as MessageLogPayload;
}

describe("heldResendPredicate", () => {
  const marker = { groupIdB64: OLD_B64, messageId: 41 };

  it("the lineage fixture reads OLD and NEW as one conversation", () => {
    const p = port(null);
    expect(p.conversationHexOf(NEW)).toBe(p.conversationHexOf(OLD));
    expect(p.segmentIndexOf(OLD)).toBeLessThan(p.segmentIndexOf(NEW));
    expect(p.conversationHexOf(OTHER)).not.toBe(p.conversationHexOf(NEW));
  });

  it("match: another device of this account holds its own original, so the re-send is dropped", async () => {
    const p = port(logWith(OLD, [row(41, "outgoing")]));
    const e = received("own", marker);
    const held = await heldResendPredicate([e], p);
    expect(held(e)).toBe(true);
  });
  it("match through a received row: the stored author is the sender", async () => {
    const p = port(logWith(OLD, [row(41, "incoming", "peer")]));
    const e = received("peer", marker);
    expect((await heldResendPredicate([e], p))(e)).toBe(true);
  });
  it("no match: the named row is not held (the person who reset), so the message is drawn", async () => {
    const p = port(logWith(OLD, [row(40, "outgoing")]));
    const e = received("own", marker);
    expect((await heldResendPredicate([e], p))(e)).toBe(false);
  });
  it("other author: a marker naming somebody else's row hides nothing", async () => {
    const p = port(logWith(OLD, [row(41, "incoming", "victim")]));
    const e = received("attacker", marker);
    expect((await heldResendPredicate([e], p))(e)).toBe(false);
  });
  it("other conversation: a marker naming a row of another conversation hides nothing", async () => {
    const p = port(logWith(OTHER, [row(41, "outgoing")]));
    const e = received("own", { groupIdB64: base64Encode(OTHER), messageId: 41 });
    expect((await heldResendPredicate([e], p))(e)).toBe(false);
  });
  it("a marker naming the receiving group itself hides nothing", async () => {
    const p = port(logWith(NEW, [row(41, "outgoing")]));
    const e = received("own", { groupIdB64: base64Encode(NEW), messageId: 41 });
    expect((await heldResendPredicate([e], p))(e)).toBe(false);
  });
  it("an unattributable sender and a junk group id hide nothing", async () => {
    const p = port(logWith(OLD, [row(41, "outgoing")]));
    const a = received(undefined, marker);
    const b = received("own", { groupIdB64: "%%%", messageId: 41 });
    const held = await heldResendPredicate([a, b], p);
    expect(held(a)).toBe(false);
    expect(held(b)).toBe(false);
  });
  it("a batch with no marker never reads the log", async () => {
    const p = port(logWith(OLD, [row(41, "outgoing")]));
    const e = received("own");
    const held = await heldResendPredicate([e], p);
    expect(held(e)).toBe(false);
    expect(p.loads).toBe(0);
  });
  it("a log that cannot be read drops nothing", async () => {
    const e = received("own", marker);
    const failing = port(null, {
      loadLog: async () => {
        throw new Error("idb");
      },
    });
    expect((await heldResendPredicate([e], failing))(e)).toBe(false);
    expect((await heldResendPredicate([e], port(null)))(e)).toBe(false);
  });
});
