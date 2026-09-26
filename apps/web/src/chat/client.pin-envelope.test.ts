// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0052 decision 3 — the FOREGROUND receive loop lifts a shared-pin
 * envelope out of the timeline, binds it to the sender's account key
 * resolved at receipt (C-68), and never renders it — not even a malformed
 * one.
 *
 * Same posture as `client.group-name-envelope.test.ts`: the REAL wasm core
 * composes and parses every envelope; only the DS and the MLS group are
 * stand-ins, and neither has an opinion about plaintext.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasmNs from "../../wasm/lakko_ffi_wasm";
import { ChatClient, type IncomingChatEvent } from "./client";
import { sniffPin } from "./pin-envelope";
import type { DsClient } from "../ds/client";
import type { PushClient } from "../push/client";
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

const push = (): PushClient => ({ notifyGroup: vi.fn(async () => {}) }) as unknown as PushClient;

let nextGid = 0;
function freshGid(): Uint8Array {
  nextGid += 1;
  return new Uint8Array([0xd2, nextGid]);
}

async function classify(senderLeaf: number, ...payloads: Uint8Array[]): Promise<IncomingChatEvent[]> {
  const chat = new ChatClient(dsWith(...payloads), { identityProvider: () => IDENTITY_TOKEN }, push());
  return (await chat.pollMessages(null, group(freshGid(), senderLeaf), 0)).events;
}

describe("ADR-0052 — a shared pin is filtered in the foreground loop", () => {
  it("is classified as a pin carrying the decoded action", async () => {
    const [ev] = await classify(3, wasm.composePinEnvelope(42n, true, 1_700n));
    expect(ev?.kind).toBe("pin");
    if (ev?.kind !== "pin") throw new Error("unreachable");
    expect(ev.pin).toEqual({ messageId: 42, pinned: true, updatedAtMs: 1_700n });
    expect(ev.senderKey).toBe(base64Encode(SENDER_KEY));
  });

  it("an unpin is a pin envelope too", async () => {
    const [ev] = await classify(3, wasm.composePinEnvelope(42n, false, 1_701n));
    if (ev?.kind !== "pin") throw new Error("expected a pin");
    expect(ev.pin).toEqual({ messageId: 42, pinned: false, updatedAtMs: 1_701n });
  });

  it("an unattributable sender carries no key (the group gate refuses it)", async () => {
    const [ev] = await classify(9, wasm.composePinEnvelope(1n, true, 1n));
    if (ev?.kind !== "pin") throw new Error("expected a pin");
    expect(ev.senderKey).toBeUndefined();
  });

  it("never reaches the timeline as text", async () => {
    const [ev] = await classify(3, wasm.composePinEnvelope(7n, true, 2n));
    expect(ev?.kind).not.toBe("application");
  });

  it("the bytes would render as a chat bubble without the arm (the defect)", async () => {
    const parsed = wasm.parseBodyEnvelope(wasm.composePinEnvelope(7n, true, 2n));
    expect(parsed.kind).toBe("chat");
    expect(parsed.text).toContain("lakko/pin/v1");
  });

  it("a malformed envelope is dropped, not rendered", async () => {
    const good = wasm.composePinEnvelope(7n, true, 3n);
    const bad = new Uint8Array(good.length + 1);
    bad.set(good, 0);
    bad[good.length] = 0x41;
    const [ev] = await classify(3, bad);
    expect(ev?.kind).toBe("pin");
    if (ev?.kind !== "pin") throw new Error("unreachable");
    expect(ev.pin).toBeUndefined();
  });

  it("a chat message in the same batch is untouched", async () => {
    const chat = new TextEncoder().encode(JSON.stringify({ v: 1, text: "hello", attachments: [] }));
    const events = await classify(3, wasm.composePinEnvelope(7n, true, 4n), chat);
    expect(events.map((e) => e.kind)).toEqual(["pin", "application"]);
  });

  it("the wire is the fixed length the service worker peeks at", () => {
    const bytes = wasm.composePinEnvelope(7n, true, 4n);
    expect(bytes.length).toBe(31);
    expect(bytes[0]).toBe(0x6c);
  });
});

describe("sniffPin", () => {
  it("refuses a message id past what a DS id can be, rather than rounding it", () => {
    const huge = BigInt(Number.MAX_SAFE_INTEGER) + 1n;
    expect(sniffPin(wasm, wasm.composePinEnvelope(huge, true, 1n))).toEqual({ kind: "malformed" });
    expect(sniffPin(wasm, wasm.composePinEnvelope(BigInt(Number.MAX_SAFE_INTEGER), true, 1n)).kind).toBe("pin");
  });

  it("a group name is not a pin (cross-kind isolation)", () => {
    expect(sniffPin(wasm, wasm.composeGroupNameEnvelope("Trio", 1n))).toEqual({ kind: "none" });
  });

  it("a plain text starting with `l` is not a pin", () => {
    expect(sniffPin(wasm, new TextEncoder().encode("lunch at six?"))).toEqual({ kind: "none" });
  });
});
