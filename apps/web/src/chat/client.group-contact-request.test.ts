// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/who-can-find-me.md` section 6.4 - the journal poll consumes a
 * contact request sealed inside the group at receipt (never an event, never a
 * bubble) and moves its cursor past the row only once the request was written:
 * an opened request is never lost to a reload between the decrypt and the
 * write. The envelope is the REAL core's; the DS and the group are stand-ins,
 * as in `client.service-envelopes.test.ts`.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { ChatClient } from "./client";
import type { DsClient } from "../ds/client";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import {
  deliverGroupContactRequest,
  groupRequestHandling,
  installGroupContactRequestSink,
  resetUnhandledForTest,
  type ArrivedGroupContactRequest,
} from "../contacts/group-contact-request";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

afterEach(() => {
  installGroupContactRequestSink(null);
  resetUnhandledForTest();
});

let nextGid = 0;
function freshGid(): Uint8Array {
  nextGid += 1;
  return new Uint8Array([0xc7, nextGid]);
}

function group(groupId: Uint8Array): WasmGroup {
  return {
    get groupId(): Uint8Array {
      return groupId;
    },
    processIncomingVerifyingRoles: (_id: unknown, payload: Uint8Array) => ({
      kind: "Application",
      applicationData: payload,
      senderLeaf: 3,
    }),
  } as unknown as WasmGroup;
}

function envelope(): Uint8Array {
  const sender = new wasm.Identity("sender");
  const recipient = new wasm.Identity("recipient");
  try {
    const request = wasm.ContactRequest.compose(
      sender.ed25519Seed,
      new Uint8Array(16).fill(1),
      recipient.x25519PublicKey,
      new Uint8Array(32).fill(2),
      1n,
      new Uint8Array(0),
      new Uint8Array(0),
      undefined,
      new Uint8Array(0),
      new Uint8Array(0),
      new Uint8Array(0),
    );
    return wasm.composeGroupContactRequestEnvelope(recipient.x25519PublicKey, request);
  } finally {
    sender.free();
    recipient.free();
  }
}

async function poll(sinkResult: boolean): Promise<{ outcome: string; cursor: number; kinds: string[]; seen: number }> {
  let seen = 0;
  installGroupContactRequestSink(async () => {
    seen += 1;
    return sinkResult;
  });
  const ds = {
    fetchGroup: vi.fn(async () => ({
      messages: [{ id: 5, payload: envelope(), createdAt: "2026-11-05T00:00:00Z" }],
      hasMore: false,
    })),
  } as unknown as DsClient;
  const chat = new ChatClient(ds, { identityProvider: () => IDENTITY_TOKEN, onPersist: async () => undefined });
  const res = await chat.pollMessages(null, group(freshGid()), 0);
  return { outcome: res.outcome, cursor: res.cursor, kinds: res.events.map((e) => e.kind), seen };
}

describe("the journal poll and a contact request sealed inside the group", () => {
  it("is consumed at receipt: never an event, and the cursor moves once it is written", async () => {
    const res = await poll(true);
    expect(res.seen).toBe(1);
    expect(res.kinds).toEqual([]);
    expect(res.outcome).toBe("applied");
    expect(res.cursor).toBe(5);
  });

  it("holds the cursor when the request could not be written yet", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const res = await poll(false);
    warn.mockRestore();
    expect(res.outcome).toBe("persist-held");
    expect(res.cursor).toBe(0);
  });
});

describe("a request not handled yet is tried again", () => {
  const arrived: ArrivedGroupContactRequest = {
    groupIdB64: "Zw==",
    groupId: new Uint8Array([1]),
    journalMessageId: 9,
    createdAt: "",
    plaintext: new Uint8Array([0x6c]),
    senderAccountKey: new Uint8Array(32),
  };

  it("with no handler yet, on the handler's installation", async () => {
    expect(await deliverGroupContactRequest(arrived)).toBe(false);
    const seen: number[] = [];
    installGroupContactRequestSink(async (a) => {
      seen.push(a.journalMessageId);
      return true;
    });
    await vi.waitFor(() => expect(seen).toEqual([9]));
  });

  it("after a failed write, on the next delivery, once", async () => {
    let ok = false;
    const seen: number[] = [];
    installGroupContactRequestSink(async (a) => {
      seen.push(a.journalMessageId);
      return ok;
    });
    expect(await deliverGroupContactRequest(arrived)).toBe(false);
    ok = true;
    expect(await deliverGroupContactRequest({ ...arrived, journalMessageId: 10 })).toBe(true);
    expect(seen).toEqual([9, 9, 10]);
    expect(await deliverGroupContactRequest({ ...arrived, journalMessageId: 11 })).toBe(true);
    expect(seen).toEqual([9, 9, 10, 11]);
  });
});

describe("a contact with no one-to-one here is asked through a Requests row", () => {
  it("only a contact this device talks to in a one-to-one is answered silently", () => {
    expect(groupRequestHandling({ senderIsContact: true, holdsOneToOne: true })).toBe("answer-silently");
    expect(groupRequestHandling({ senderIsContact: true, holdsOneToOne: false })).toBe("list");
    expect(groupRequestHandling({ senderIsContact: false, holdsOneToOne: false })).toBe("list");
    expect(groupRequestHandling({ senderIsContact: false, holdsOneToOne: true })).toBe("list");
  });
});
