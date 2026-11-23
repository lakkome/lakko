// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Call rows in the history bundle (`docs/ux/calls.md` section 6, placeholder
 * version 7): a device added to the account gets the calls its conversations
 * had, in its call log, once.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import * as wasm from "../../wasm/lakko_ffi_wasm";
import init from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import { WebDeviceCipher, type DeviceCipher } from "../auth/device-cipher";
import type { CallLogEntry } from "../calls/call-log";
import {
  callHistoryMessageId,
  historyCallEntryId,
  registerLiveCallLogStore,
} from "../calls/call-log-history";
import { IdbCallLogBackend, loadCallLogRows } from "../calls/call-log-idb";
import { CallLogStore } from "../calls/call-log-store";
import { loadMessageLog, saveMessageLog, type PersistedMessage } from "../chat/message-log-store";
import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import {
  applyHistoryBundleToLog,
  buildHistoryBundleFromLog,
  decodeHistoryRow,
  mergeRowsAndCalls,
} from "./history-bundle";

const SENDER = "c2VuZGVyLWFjY291bnQtMDA=";
const JOINER = "am9pbmVyLWFjY291bnQtMDA=";
const GROUP_HEX = "ab".repeat(32);
const GROUP_B64 = base64Encode(new Uint8Array(32).fill(0xab));
const OTHER_HEX = "cd".repeat(32);
const OTHER_B64 = base64Encode(new Uint8Array(32).fill(0xcd));
const SECRET = new Uint8Array(32).fill(7);
const T0 = 1_700_000_000_000;

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

beforeEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = new MemoryStorage();
  g["indexedDB"] = new FakeIndexedDb();
});

afterEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["indexedDB"];
});

async function deviceKey(): Promise<DeviceCipher> {
  return new WebDeviceCipher(
    await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]),
  );
}

const row = (id: number, direction: PersistedMessage["direction"], text: string): PersistedMessage => ({
  messageId: String(id),
  direction,
  text,
  attachments: [],
  createdAt: new Date(T0 + id * 1000).toISOString(),
});

const OUTGOING: CallLogEntry = {
  id: "0123456789abcdef0123456789abcdef",
  outcome: "outgoing",
  video: true,
  startedAtMs: T0 + 1500,
  durationMs: 42_700,
};
const DECLINED: CallLogEntry = {
  id: "fedcba9876543210fedcba9876543210",
  outcome: "declined",
  video: false,
  startedAtMs: T0 + 2500,
  durationMs: null,
};

async function senderWith(
  messages: readonly PersistedMessage[] | null,
  calls: ReadonlyMap<string, readonly CallLogEntry[]>,
): Promise<Uint8Array> {
  const key = await deviceKey();
  if (messages !== null) {
    await saveMessageLog(SENDER, key, {
      version: 1,
      groups: { [GROUP_HEX]: { cursor: "3", messages: [...messages] } },
    });
  }
  await new IdbCallLogBackend(async () => key).save(SENDER, { groups: calls, standalone: [] });
  const sealed = await buildHistoryBundleFromLog({ accountIdB64: SENDER, deviceKey: key, secret: SECRET });
  if (sealed === null) throw new Error("no bundle");
  return sealed;
}

function carriedRows(sealed: Uint8Array) {
  const opened = wasm.openHistoryBundleStructured(SECRET, sealed);
  return (opened.groups() as wasm.OpenedHistoryGroup[]).flatMap((g) => g.messages() as wasm.OpenedHistoryMessage[]);
}

describe("call rows reach a device added to the account", () => {
  it("the sender carries each call with its outcome, kind and whole seconds", async () => {
    const sealed = await senderWith(
      [row(1, "incoming", "hi"), row(3, "outgoing", "bye")],
      new Map([[GROUP_B64, [OUTGOING, DECLINED]]]),
    );
    const rows = carriedRows(sealed).map((m) => decodeHistoryRow(wasm, m));
    expect(rows.map((r) => r?.kind)).toEqual(["message", "call", "call", "message"]);
    const placeholders = carriedRows(sealed).map((m) => wasm.decodeHistoryPlaceholder(m.payloadBytes));
    const outgoing = placeholders[1];
    const declined = placeholders[2];
    expect(outgoing?.callOutcome).toBe(wasm.CallRecordOutcome.Outgoing);
    expect(outgoing?.callVideo).toBe(true);
    expect(outgoing?.callDurationSecs).toBe(42);
    expect(outgoing?.messageId).toBe(callHistoryMessageId(OUTGOING.id));
    expect(outgoing?.messageId).toBeGreaterThanOrEqual(0x8000_0000_0000_0000n);
    expect(declined?.callOutcome).toBe(wasm.CallRecordOutcome.Declined);
    expect(declined?.callVideo).toBe(false);
    expect(declined?.callDurationSecs).toBeUndefined();
    expect(placeholders[0]?.callOutcome).toBeUndefined();
  });

  it("the joiner files calls into the call log, messages into the message log", async () => {
    const sealed = await senderWith(
      [row(1, "incoming", "hi"), row(3, "outgoing", "bye")],
      new Map([[GROUP_B64, [OUTGOING, DECLINED]]]),
    );
    const key = await deviceKey();
    const applied = await applyHistoryBundleToLog({ accountIdB64: JOINER, deviceKey: key, secret: SECRET, sealedBytes: sealed });
    expect(applied).toBe(4);
    const log = await loadMessageLog(JOINER, key);
    const slot = log?.groups[GROUP_HEX];
    expect(slot?.messages.map((m) => m.text)).toEqual(["hi", "bye"]);
    // The history floor is the newest message, never a call row's id.
    expect(slot?.syncedThroughMessageId).toBe("3");
    const calls = (await loadCallLogRows(JOINER, key))?.groups.get(GROUP_B64) ?? [];
    expect(calls).toEqual([
      {
        id: historyCallEntryId(callHistoryMessageId(OUTGOING.id)),
        outcome: "outgoing",
        video: true,
        startedAtMs: OUTGOING.startedAtMs,
        durationMs: 42_000,
      },
      {
        id: historyCallEntryId(callHistoryMessageId(DECLINED.id)),
        outcome: "declined",
        video: false,
        startedAtMs: DECLINED.startedAtMs,
        durationMs: null,
      },
    ]);
    // Never the id of a live call (32 hex characters).
    for (const c of calls) expect(c.id.startsWith("h-")).toBe(true);
  });

  it("applying the same bundle again adds nothing", async () => {
    const sealed = await senderWith([row(1, "incoming", "hi")], new Map([[GROUP_B64, [OUTGOING, DECLINED]]]));
    const key = await deviceKey();
    const apply = (): Promise<number> =>
      applyHistoryBundleToLog({ accountIdB64: JOINER, deviceKey: key, secret: SECRET, sealedBytes: sealed });
    expect(await apply()).toBe(3);
    expect(await apply()).toBe(0);
    expect((await loadCallLogRows(JOINER, key))?.groups.get(GROUP_B64)).toHaveLength(2);
    expect((await loadMessageLog(JOINER, key))?.groups[GROUP_HEX]?.messages).toHaveLength(1);
  });

  it("a call that came from a bundle is sent on under the same id", async () => {
    const sealed = await senderWith([row(1, "incoming", "hi")], new Map([[GROUP_B64, [OUTGOING]]]));
    const key = await deviceKey();
    await applyHistoryBundleToLog({ accountIdB64: JOINER, deviceKey: key, secret: SECRET, sealedBytes: sealed });
    const relayed = await buildHistoryBundleFromLog({ accountIdB64: JOINER, deviceKey: key, secret: SECRET });
    if (relayed === null) throw new Error("no bundle");
    const third = await deviceKey();
    await applyHistoryBundleToLog({ accountIdB64: "dGhpcmQ=", deviceKey: third, secret: SECRET, sealedBytes: relayed });
    const ids = ((await loadCallLogRows("dGhpcmQ=", third))?.groups.get(GROUP_B64) ?? []).map((c) => c.id);
    expect(ids).toEqual([historyCallEntryId(callHistoryMessageId(OUTGOING.id))]);
  });

  it("a conversation with calls and no messages makes no message slot", async () => {
    const sealed = await senderWith(null, new Map([[OTHER_B64, [DECLINED]]]));
    const key = await deviceKey();
    expect(await applyHistoryBundleToLog({ accountIdB64: JOINER, deviceKey: key, secret: SECRET, sealedBytes: sealed })).toBe(1);
    expect((await loadMessageLog(JOINER, key))?.groups[OTHER_HEX]).toBeUndefined();
    expect((await loadCallLogRows(JOINER, key))?.groups.get(OTHER_B64)).toHaveLength(1);
  });

  it("goes through the session's store when one is bound to the account", async () => {
    const sealed = await senderWith([row(1, "incoming", "hi")], new Map([[GROUP_B64, [OUTGOING]]]));
    const key = await deviceKey();
    const store = new CallLogStore(new IdbCallLogBackend(async () => key));
    await store.open(JOINER);
    const unregister = registerLiveCallLogStore(store);
    try {
      expect(await applyHistoryBundleToLog({ accountIdB64: JOINER, deviceKey: key, secret: SECRET, sealedBytes: sealed })).toBe(2);
      expect(store.of(GROUP_B64).map((c) => c.outcome)).toEqual(["outgoing"]);
      await store.flush();
      expect((await loadCallLogRows(JOINER, key))?.groups.get(GROUP_B64)).toHaveLength(1);
    } finally {
      unregister();
    }
  });
});

describe("messages and calls share the group's place in the bundle", () => {
  it("orders them by time, a tie going to the message, and keeps message order", () => {
    const merged = mergeRowsAndCalls(
      [row(1, "incoming", "a"), { ...row(2, "incoming", "b"), createdAt: "" }, row(3, "incoming", "c")],
      [DECLINED, OUTGOING, { ...OUTGOING, id: "11".repeat(16), startedAtMs: T0 + 1000 }],
    );
    expect(merged.map((i) => (i.kind === "message" ? i.row.text : i.call.id.slice(0, 4)))).toEqual([
      "a",
      "b",
      "1111",
      "0123",
      "fedc",
      "c",
    ]);
  });

  it("the newest are kept when the bundle is full", async () => {
    // 1000 messages and one older call: the call is the one left out.
    const many = Array.from({ length: 1000 }, (_, i) => row(i + 10, "incoming", `m${i}`));
    const sealed = await senderWith(many, new Map([[GROUP_B64, [{ ...DECLINED, startedAtMs: T0 }]]]));
    expect(carriedRows(sealed).every((m) => decodeHistoryRow(wasm, m)?.kind === "message")).toBe(true);
  });
});

describe("the wire id of a call row", () => {
  it("is deterministic, top-bit set, and round-trips through a history id", () => {
    const id = callHistoryMessageId(OUTGOING.id);
    expect(id).toBe(0x8123_4567_89ab_cdefn);
    expect(callHistoryMessageId(OUTGOING.id)).toBe(id);
    expect(callHistoryMessageId(historyCallEntryId(id))).toBe(id);
    expect(callHistoryMessageId("not hex")).toBeGreaterThanOrEqual(0x8000_0000_0000_0000n);
  });
});
