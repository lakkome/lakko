// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0059 section 6 — the FOREGROUND receive loop lifts a shared-channels
 * list out of the timeline, binds it to the sender's key resolved at receipt
 * (C-68), and never renders it, not even a malformed one. The fixture is
 * `client.pin-envelope.test.ts`'s: the REAL wasm core composes and parses.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasmNs from "../../wasm/lakko_ffi_wasm";
import { ChatClient, type IncomingChatEvent } from "./client";
import type { DsClient } from "../ds/client";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;
let wasm: typeof wasmNs;

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
  wasm = wasmNs;
});

const SENDER_KEY = new Uint8Array(32).fill(0x7e);
const OTHER_KEY = new Uint8Array(32).fill(0x11);

function group(groupId: Uint8Array, senderLeaf = 3): WasmGroup {
  return {
    get groupId(): Uint8Array {
      return groupId;
    },
    members: () => [
      { leafIndex: 0, signaturePublicKey: OTHER_KEY },
      { leafIndex: 3, signaturePublicKey: SENDER_KEY },
    ],
    processIncomingVerifyingRoles: vi.fn((_id: unknown, payload: Uint8Array) => ({
      kind: "Application",
      applicationData: payload,
      senderLeaf,
    })),
  } as unknown as WasmGroup;
}

function dsWith(...payloads: Uint8Array[]): DsClient {
  return {
    fetchGroup: vi.fn(async () => ({
      messages: payloads.map((payload, i) => ({ id: i + 1, payload, createdAt: "2026-09-26T00:00:00Z" })),
      hasMore: false,
    })),
  } as unknown as DsClient;
}

let nextGid = 0;
function freshGid(): Uint8Array {
  nextGid += 1;
  return new Uint8Array([0xd5, nextGid]);
}

async function classify(senderLeaf: number, ...payloads: Uint8Array[]): Promise<IncomingChatEvent[]> {
  const chat = new ChatClient(dsWith(...payloads), { identityProvider: () => IDENTITY_TOKEN });
  return (await chat.pollMessages(null, group(freshGid(), senderLeaf), 0)).events;
}

const oneChannel = (stamp: bigint): Uint8Array =>
  wasm.composeSharedChannelsEnvelope(stamp, new Uint8Array(16).fill(0xaa), ["alpha"], ["Alpha"]);

describe("ADR-0059 — a shared-channels list is filtered in the foreground loop", () => {
  it("is classified as a list carrying the sender's key", async () => {
    const [ev] = await classify(3, oneChannel(1_700n));
    expect(ev?.kind).toBe("shared-channels");
    if (ev?.kind !== "shared-channels") throw new Error("unreachable");
    expect(ev.list?.updatedAtMs).toBe(1_700n);
    expect(ev.list?.channels).toEqual([{ channelIdHex: "aa".repeat(16), handle: "alpha", name: "Alpha" }]);
    expect(ev.senderKey).toBe(base64Encode(SENDER_KEY));
  });

  it("the bytes would render as a chat bubble without the arm (the defect)", () => {
    const parsed = wasm.parseBodyEnvelope(oneChannel(1n));
    expect(parsed.kind).toBe("chat");
    expect(parsed.text).toContain("lakko/shared-channels/v1");
  });

  it("a malformed list is dropped, not rendered", async () => {
    const good = oneChannel(3n);
    const bad = new Uint8Array(good.length + 1);
    bad.set(good, 0);
    bad[good.length] = 0x41;
    const [ev] = await classify(3, bad);
    expect(ev?.kind).toBe("shared-channels");
    if (ev?.kind !== "shared-channels") throw new Error("unreachable");
    expect(ev.list).toBeUndefined();
  });

  it("a chat message and a pin in the same batch are untouched", async () => {
    const chat = new TextEncoder().encode(JSON.stringify({ v: 1, text: "hello", attachments: [] }));
    const events = await classify(3, oneChannel(4n), wasm.composePinEnvelope(7n, true, 4n), chat);
    expect(events.map((e) => e.kind)).toEqual(["shared-channels", "pin", "application"]);
  });
});
