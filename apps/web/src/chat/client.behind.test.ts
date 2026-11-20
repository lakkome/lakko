// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 phase 2 through the real poll (`ChatClient.pollMessages`) and the
 * real core verdict: the poll feeds the behind state, a device behind stops
 * sending repeat envelopes and, once confirmed, messages; refused reads count;
 * a re-entry read-from mark reads again from the commit that brought the
 * device back.
 *
 * The double is `client.registered-commit.test.ts`'s: a JSON wire whose rows
 * say what they are, the two framing readers mocked to read the same JSON.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./commit-outbox", () => ({
  markAcked: async () => {},
  loadUnacked: async () => [],
}));

vi.mock("../../wasm/lakko_ffi_wasm", async (importOriginal) => {
  const actual = await importOriginal<Record<string, unknown>>();
  const read = (bytes: Uint8Array): Row | undefined => {
    try {
      return JSON.parse(new TextDecoder().decode(bytes)) as Row;
    } catch {
      return undefined;
    }
  };
  return {
    ...actual,
    epochOfMessage: (bytes: Uint8Array) => {
      const row = read(bytes);
      return row === undefined ? undefined : BigInt(row.sealed);
    },
    epochAfterMessage: (bytes: Uint8Array) => {
      const row = read(bytes);
      if (row === undefined) return undefined;
      return BigInt(row.t === "commit" ? row.sealed + 1 : row.sealed);
    },
  };
});

import init from "../../wasm/lakko_ffi_wasm";
import { ChatClient, ConversationBehindError } from "./client";
import { conversationBehind } from "./conversation-behind";
import { reentryReadFrom, setReentryReadFrom } from "./reentry-store";
import { bindContactsAccount } from "../contacts/local-store";
import { DsHttpError, type DsClient } from "../ds/client";
import { registeredCommitHolds } from "./registered-commit-hold";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

type Row = { readonly t: "commit" | "app"; readonly sealed: number };

const ACCOUNT = "YmVoaW5kLWFjY291bnQtMDAwMA==";
const IDENTITY_TOKEN = {} as unknown as WasmIdentity;
let nextGroupId = 0x51;

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function device(opts: { latestEpoch: number; refuseReads?: () => boolean }) {
  nextGroupId += 1;
  const groupId = new Uint8Array([nextGroupId, 0x92]);
  let cursor = 0;
  let nextId = 100;
  let sealed = 0;
  const froms: number[] = [];
  const journal: { id: number; payload: Uint8Array; createdAt: string }[] = [];
  const group = {
    get groupId(): Uint8Array {
      return groupId;
    },
    get epoch(): bigint {
      return 0n;
    },
    members: () => [],
    hasStagedSweep: () => false,
    createApplicationMessage: () => {
      sealed += 1;
      return new Uint8Array([1]);
    },
    processIncomingVerifyingRoles: (_id: unknown, bytes: Uint8Array): unknown => {
      const row = JSON.parse(new TextDecoder().decode(bytes)) as Row;
      if (row.sealed !== 0) throw new Error("ValidationError(WrongEpoch)");
      return { kind: "Application", applicationData: new TextEncoder().encode("{}"), senderLeaf: undefined };
    },
  } as unknown as WasmGroup;
  const ds = {
    fetchGroup: async (_gid: Uint8Array, o: { after: number }) => {
      if (opts.refuseReads?.() === true) throw new DsHttpError(401, "group", "proof required", false);
      froms.push(o.after);
      return {
        messages: journal.filter((r) => r.id > o.after),
        hasMore: false,
        latestEpoch: opts.latestEpoch,
        registeredCommits: new Map<bigint, number>(),
      };
    },
    publishGroup: async () => ({ id: 999, createdAt: "2026-11-19T12:00:00Z" }),
  } as unknown as DsClient;
  const chat = new ChatClient(ds, { identityProvider: () => IDENTITY_TOKEN, onPersist: async () => {} });
  return {
    group,
    chat,
    gid: btoa(String.fromCharCode(...groupId)),
    cursorRef: { read: () => cursor, advance: (to: number) => void (cursor = to) },
    cursor: () => cursor,
    sealedCount: () => sealed,
    froms,
    push: (row: Row) => {
      nextId += 1;
      journal.push({ id: nextId, payload: new TextEncoder().encode(JSON.stringify(row)), createdAt: "2026-11-19T11:00:00Z" });
      return nextId;
    },
  };
}

let data: Map<string, string>;

beforeEach(() => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.spyOn(console, "error").mockImplementation(() => {});
  vi.spyOn(console, "info").mockImplementation(() => {});
  data = new Map();
  (globalThis as unknown as { localStorage: Storage }).localStorage = {
    get length() {
      return data.size;
    },
    clear: () => data.clear(),
    getItem: (k: string) => data.get(k) ?? null,
    key: (i: number) => [...data.keys()][i] ?? null,
    removeItem: (k: string) => void data.delete(k),
    setItem: (k: string, v: string) => void data.set(k, String(v)),
  };
  bindContactsAccount(ACCOUNT);
  conversationBehind.resetForTests();
});

afterEach(() => {
  bindContactsAccount(null);
  registeredCommitHolds.clear();
  vi.restoreAllMocks();
});

describe("ADR-0091 phase 2 in the poll", () => {
  it("a drained read with the journal ahead suspects; a failed row above the local epoch confirms", async () => {
    const d = device({ latestEpoch: 3 });
    // Nothing to read, journal at 3, device at 0: suspected.
    await d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    expect(conversationBehind.state(d.gid).kind).toBe("suspected");
    // A row someone sealed at epoch 2 fails and is skipped: evidence.
    d.push({ t: "app", sealed: 2 });
    await d.chat.pollMessages(null, d.group, d.cursor(), 0, d.cursorRef);
    expect(conversationBehind.gap(d.gid).gapFromMs).toBe(Date.parse("2026-11-19T11:00:00Z"));
    await d.chat.pollMessages(null, d.group, d.cursor(), 0, d.cursorRef);
    expect(conversationBehind.isConfirmed(d.gid)).toBe(true);
  });

  it("a suspected device sends no repeat envelope, typing or receipt; nothing is sealed", async () => {
    const d = device({ latestEpoch: 3 });
    await d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    expect(conversationBehind.stopsActing(d.gid)).toBe(true);
    await expect(d.chat.broadcastControl(null, d.group, new Uint8Array([1, 2]))).rejects.toBeInstanceOf(ConversationBehindError);
    expect(d.sealedCount()).toBe(0);
  });

  it("only a confirmed device refuses a message", async () => {
    const d = device({ latestEpoch: 3 });
    await d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    // Suspected: the send is attempted (its refusal would be evidence).
    await expect(d.chat.sendMessage(null, d.group, new Uint8Array([1]))).resolves.toBeDefined();
    expect(d.sealedCount()).toBe(1);
    d.push({ t: "app", sealed: 2 });
    await d.chat.pollMessages(null, d.group, d.cursor(), 0, d.cursorRef);
    await d.chat.pollMessages(null, d.group, d.cursor(), 0, d.cursorRef);
    await expect(d.chat.sendMessage(null, d.group, new Uint8Array([1]))).rejects.toBeInstanceOf(ConversationBehindError);
    expect(d.sealedCount()).toBe(1);
  });

  it("three refused reads at the device's key confirm", async () => {
    let refuse = true;
    const d = device({ latestEpoch: 3, refuseReads: () => refuse });
    for (let i = 0; i < 3; i++) {
      await expect(d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef)).rejects.toThrow();
    }
    expect(conversationBehind.isConfirmed(d.gid)).toBe(true);
    refuse = false;
  });

  it("one refused read holds a repeat envelope or typing until a read is answered (live walk 3)", async () => {
    let refuse = true;
    const d = device({ latestEpoch: 0, refuseReads: () => refuse });
    await expect(d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef)).rejects.toThrow();
    expect(conversationBehind.state(d.gid).kind).toBe("in-step");
    await expect(d.chat.broadcastControl(null, d.group, new Uint8Array([1, 2]))).rejects.toBeInstanceOf(ConversationBehindError);
    expect(d.sealedCount()).toBe(0);
    refuse = false;
    await d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    await expect(d.chat.broadcastControl(null, d.group, new Uint8Array([1, 2]))).resolves.toBeDefined();
    expect(d.sealedCount()).toBe(1);
  });

  it("a person's own action is attempted after a refused read and held only while behind", async () => {
    let refuse = true;
    let latest = 0;
    const d = device({
      get latestEpoch() {
        return latest;
      },
      refuseReads: () => refuse,
    } as unknown as { latestEpoch: number; refuseReads: () => boolean });
    await expect(d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef)).rejects.toThrow();
    // One refused read: an edit, reaction or rename is still attempted (its
    // refusal would be evidence, and the person sees it fail).
    await expect(d.chat.broadcastControl(null, d.group, new Uint8Array([1]), { personAction: true })).resolves.toBeDefined();
    expect(d.sealedCount()).toBe(1);
    // Suspected: held, refused before anything is sealed.
    refuse = false;
    latest = 3;
    await d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    expect(conversationBehind.stopsActing(d.gid)).toBe(true);
    await expect(d.chat.broadcastControl(null, d.group, new Uint8Array([1]), { personAction: true })).rejects.toBeInstanceOf(
      ConversationBehindError,
    );
    expect(d.sealedCount()).toBe(1);
  });

  it("a journal back at the local epoch clears", async () => {
    let latest = 3;
    const d = device({
      get latestEpoch() {
        return latest;
      },
    } as unknown as { latestEpoch: number });
    await d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    expect(conversationBehind.stopsActing(d.gid)).toBe(true);
    latest = 0;
    await d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    expect(conversationBehind.state(d.gid).kind).toBe("in-step");
  });

  it("the read-from mark of a re-entry reads again from the commit, once", async () => {
    const d = device({ latestEpoch: 0 });
    const a = d.push({ t: "app", sealed: 0 });
    const b = d.push({ t: "app", sealed: 0 });
    await d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    expect(d.cursor()).toBe(b);
    setReentryReadFrom(d.gid, a - 1);
    const result = await d.chat.pollMessages(null, d.group, d.cursor(), 0, d.cursorRef);
    expect(d.froms.at(-1)).toBe(a - 1);
    expect(result.events.length).toBe(2);
    expect(d.cursor()).toBe(b);
    expect(reentryReadFrom(d.gid)).toBeUndefined();
  });
});
