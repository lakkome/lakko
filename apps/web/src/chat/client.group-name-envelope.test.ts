// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0051 — the FOREGROUND receive loop lifts a group-name envelope out of
 * the timeline, binds it to the sender's account key resolved at receipt
 * (C-68), and never renders it — not even a malformed one.
 *
 * Same posture as `client.profile-envelope.test.ts`: the REAL wasm core
 * composes and parses every envelope; only the DS and the MLS group are
 * stand-ins, and neither has an opinion about plaintext.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasmNs from "../../wasm/lakko_ffi_wasm";
import { ChatClient, type IncomingChatEvent } from "./client";
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
      messages: payloads.map((payload, i) => ({ id: i + 1, payload, createdAt: "2026-09-17T00:00:00Z" })),
      hasMore: false,
    })),
  } as unknown as DsClient;
}

const push = (): PushClient => ({ notifyGroup: vi.fn(async () => {}) }) as unknown as PushClient;

let nextGid = 0;
function freshGid(): Uint8Array {
  nextGid += 1;
  return new Uint8Array([0xd1, nextGid]);
}

async function classify(senderLeaf: number, ...payloads: Uint8Array[]): Promise<IncomingChatEvent[]> {
  const chat = new ChatClient(dsWith(...payloads), { identityProvider: () => IDENTITY_TOKEN }, push());
  return (await chat.pollMessages(null, group(freshGid(), senderLeaf), 0)).events;
}

describe("ADR-0051 — a group name is filtered in the foreground loop", () => {
  it("is classified as a group name carrying the decoded value", async () => {
    const [ev] = await classify(3, wasm.composeGroupNameEnvelope("Trio", 1_700n));
    expect(ev?.kind).toBe("group-name");
    if (ev?.kind !== "group-name") throw new Error("unreachable");
    expect(ev.groupName).toEqual({ name: "Trio", updatedAtMs: 1_700n });
    expect(ev.senderKey).toBe(base64Encode(SENDER_KEY));
  });

  it("a clear (empty name) is a group name too", async () => {
    const [ev] = await classify(3, wasm.composeGroupNameEnvelope("  ", 1_701n));
    if (ev?.kind !== "group-name") throw new Error("expected a group name");
    expect(ev.groupName).toEqual({ name: "", updatedAtMs: 1_701n });
  });

  it("an unattributable sender carries no key (the gate refuses it)", async () => {
    const [ev] = await classify(9, wasm.composeGroupNameEnvelope("Trio", 1n));
    if (ev?.kind !== "group-name") throw new Error("expected a group name");
    expect(ev.senderKey).toBeUndefined();
  });

  it("never reaches the timeline as text", async () => {
    const [ev] = await classify(3, wasm.composeGroupNameEnvelope("Zed Unique Group", 2n));
    const asText = (ev as { text?: string } | undefined)?.text ?? "";
    expect(asText).not.toContain("Zed Unique Group");
    expect(ev?.kind).not.toBe("application");
  });

  it("the bytes would render as a chat bubble without the arm (the defect)", async () => {
    const parsed = wasm.parseBodyEnvelope(wasm.composeGroupNameEnvelope("Zed Unique Group", 2n));
    expect(parsed.kind).toBe("chat");
    expect(parsed.text).toContain("Zed Unique Group");
  });

  it("a malformed envelope is dropped, not rendered", async () => {
    const good = wasm.composeGroupNameEnvelope("Trio", 3n);
    const bad = new Uint8Array(good.length + 1);
    bad.set(good, 0);
    bad[good.length] = 0x41;
    const [ev] = await classify(3, bad);
    expect(ev?.kind).toBe("group-name");
    if (ev?.kind !== "group-name") throw new Error("unreachable");
    expect(ev.groupName).toBeUndefined();
  });

  it("a chat message in the same batch is untouched", async () => {
    const chat = new TextEncoder().encode(JSON.stringify({ v: 1, text: "hello", attachments: [] }));
    const events = await classify(3, wasm.composeGroupNameEnvelope("Trio", 4n), chat);
    expect(events.map((e) => e.kind)).toEqual(["group-name", "application"]);
  });
});
