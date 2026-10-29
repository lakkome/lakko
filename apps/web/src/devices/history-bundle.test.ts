// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The history bundle carries no bookkeeping system rows (`[mls <kind>]`):
 * the new web device restored every one as `Members changed` (live
 * 2026-10-28, a one-to-one after a pairing). Neither built nor applied.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import * as wasm from "../../wasm/lakko_ffi_wasm";
import init from "../../wasm/lakko_ffi_wasm";
import { loadMessageLog, saveMessageLog, type PersistedMessage } from "../chat/message-log-store";
import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import { applyHistoryBundleToLog, buildHistoryBundleFromLog, isBookkeepingSystemRow } from "./history-bundle";

const SENDER = "c2VuZGVyLWFjY291bnQtMDA=";
const JOINER = "am9pbmVyLWFjY291bnQtMDA=";
const GROUP_HEX = "ab".repeat(32);
const SECRET = new Uint8Array(32).fill(7);

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

function deviceKey(): Promise<CryptoKey> {
  return crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
}

const row = (id: number, direction: PersistedMessage["direction"], text: string): PersistedMessage => ({
  messageId: String(id),
  direction,
  text,
  attachments: [],
  createdAt: new Date(1_700_000_000_000 + id).toISOString(),
});

function texts(messages: readonly PersistedMessage[] | undefined): string[] {
  return (messages ?? []).map((m) => `${m.direction}:${m.text}`);
}

describe("bookkeeping system rows in the history bundle", () => {
  it("recognises the commit and join-request rows of every client, not a call row", () => {
    expect(isBookkeepingSystemRow(row(1, "system", "[mls commit]"))).toBe(true);
    expect(isBookkeepingSystemRow(row(1, "system", "[mls external-join-proposal]"))).toBe(true);
    expect(isBookkeepingSystemRow(row(1, "system", "[mls external join]"))).toBe(true);
    expect(isBookkeepingSystemRow(row(1, "incoming", "[mls commit]"))).toBe(false);
    expect(isBookkeepingSystemRow(row(1, "system", "call:v1 ..."))).toBe(false);
  });

  it("a web sender does not put them in the bundle", async () => {
    const key = await deviceKey();
    await saveMessageLog(SENDER, key, {
      version: 1,
      groups: {
        [GROUP_HEX]: {
          cursor: "5",
          messages: [
            row(1, "system", "[mls commit]"),
            row(2, "incoming", "hello"),
            row(3, "system", "[mls external-join-proposal]"),
            row(4, "outgoing", "hi back"),
            row(5, "system", "[mls commit]"),
          ],
        },
      },
    });
    const sealed = await buildHistoryBundleFromLog({ accountIdB64: SENDER, deviceKey: key, secret: SECRET });
    if (sealed === null) throw new Error("no bundle");
    // What is on the wire, not only what a web joiner keeps.
    const opened = wasm.openHistoryBundleStructured(SECRET, sealed);
    const carried = (opened.groups() as { messages(): unknown[] }[]).flatMap((g) => g.messages());
    expect(carried).toHaveLength(2);
    const joinerKey = await deviceKey();
    expect(await applyHistoryBundleToLog({ accountIdB64: JOINER, deviceKey: joinerKey, secret: SECRET, sealedBytes: sealed })).toBe(2);
    const log = await loadMessageLog(JOINER, joinerKey);
    expect(texts(log?.groups[GROUP_HEX]?.messages)).toEqual(["incoming:hello", "outgoing:hi back"]);
  });

  it("a received bundle that carries them (another client) does not log them", async () => {
    const messages = [
      [1, 2, "[mls commit]"],
      [2, 1, "from the other person"],
      [3, 2, "[mls external join]"],
      [4, 2, "[mls commit]"],
    ] as const;
    const inputs = messages.map(([id, direction, text]) => {
      const placeholder = wasm.encodeHistoryPlaceholder(
        BigInt(id),
        direction,
        new Date(1_700_000_000_000 + id).toISOString(),
        text,
        [],
      );
      const idBytes = new Uint8Array(8);
      idBytes[7] = id;
      return new wasm.HistoryMessageInput(placeholder, 0, idBytes, BigInt(1_700_000_000_000 + id));
    });
    const groupId = new Uint8Array(32).fill(0xab);
    const sealed = wasm.sealHistoryBundleStructured(SECRET, [new wasm.HistoryGroupInput(groupId, inputs, false)], 4n);
    const key = await deviceKey();
    expect(await applyHistoryBundleToLog({ accountIdB64: JOINER, deviceKey: key, secret: SECRET, sealedBytes: sealed })).toBe(1);
    const log = await loadMessageLog(JOINER, key);
    expect(texts(log?.groups[GROUP_HEX]?.messages)).toEqual(["incoming:from the other person"]);
  });
});
