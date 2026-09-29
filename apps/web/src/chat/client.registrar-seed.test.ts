// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 obligation 4 — the FOREGROUND receive loop lifts a registrar-seed
 * envelope out of the batch, hands it to the gate AT RECEIPT with the sender's
 * account key and the LIVE role table's owner (C-68), and never renders it.
 *
 * The REAL wasm core composes the envelope and classifies it; only the DS and
 * the MLS group are stand-ins, and the gate is observed through the sink.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { ChatClient } from "./client";
import type { DsClient } from "../ds/client";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { installRegistrarSeedHandler, type RegistrarSeedArrival } from "../ds/registrar-seeds";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const OWNER_KEY = new Uint8Array(32).fill(0x0a);
const MEMBER_KEY = new Uint8Array(32).fill(0x0b);
const GID = new Uint8Array(32).fill(0x3c);

function group(senderLeaf: number, owner: Uint8Array | undefined): WasmGroup {
  return {
    get groupId(): Uint8Array {
      return GID;
    },
    roleTableOwner: () => owner,
    members: () => [
      { leafIndex: 0, signaturePublicKey: new Uint8Array(32).fill(0xa0), accountKey: OWNER_KEY },
      { leafIndex: 1, signaturePublicKey: new Uint8Array(32).fill(0xa1), accountKey: MEMBER_KEY },
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
      messages: payloads.map((payload, i) => ({ id: i + 1, payload, createdAt: "2026-09-28T00:00:00Z" })),
      hasMore: false,
    })),
  } as unknown as DsClient;
}

function envelope(): Uint8Array {
  const member = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x44));
  try {
    return wasm.composeRegistrarSeedEnvelope(GID, member.signaturePublicKey, member.x25519PublicKey, 1, new Uint8Array(32).fill(9));
  } finally {
    member.free();
  }
}

let uninstall: (() => void) | null = null;
afterEach(() => {
  uninstall?.();
  uninstall = null;
});

function capture(): RegistrarSeedArrival[] {
  const seen: RegistrarSeedArrival[] = [];
  uninstall = installRegistrarSeedHandler(async (a) => {
    seen.push(a);
    return "accept";
  });
  return seen;
}

describe("ADR-0057 — a registrar seed never reaches the timeline", () => {
  it("is consumed at receipt with the sender's account key and the live owner, and not emitted", async () => {
    const seen = capture();
    const chat = new ChatClient(dsWith(envelope(), new TextEncoder().encode("hello")), {
      identityProvider: () => IDENTITY_TOKEN,
    });
    const out = await chat.pollMessages(null, group(0, OWNER_KEY), 0);
    expect(out.events.map((e) => e.kind)).toEqual(["application"]);
    expect(seen).toHaveLength(1);
    expect(seen[0]?.senderAccountKey).toEqual(OWNER_KEY);
    expect(seen[0]?.ownerAccountKey).toEqual(OWNER_KEY);
    expect(seen[0]?.groupId).toEqual(GID);
    expect(out.cursor).toBe(2);
  });

  it("a member's envelope reaches the gate named as the member's (the gate refuses it)", async () => {
    const seen = capture();
    const chat = new ChatClient(dsWith(envelope()), { identityProvider: () => IDENTITY_TOKEN });
    const out = await chat.pollMessages(null, group(1, OWNER_KEY), 0);
    expect(out.events).toEqual([]);
    expect(seen[0]?.senderAccountKey).toEqual(MEMBER_KEY);
    expect(seen[0]?.ownerAccountKey).toEqual(OWNER_KEY);
  });

  it("with no handler installed (signed out) it is dropped, still never rendered", async () => {
    const chat = new ChatClient(dsWith(envelope()), { identityProvider: () => IDENTITY_TOKEN });
    const out = await chat.pollMessages(null, group(0, OWNER_KEY), 0);
    expect(out.events).toEqual([]);
  });
});
