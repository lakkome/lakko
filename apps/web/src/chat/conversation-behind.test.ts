// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 phase 2 section 1 and 2 - the behind store over the REAL core
 * verdict (`conversationBehindVerdict`, loaded from the built wasm).
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, beforeEach, describe, expect, it } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  ConversationBehindStore,
  wasmBehindVerdict,
  type BehindBackend,
  type BehindVerdictWasm,
} from "./conversation-behind";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function memoryBackend(): BehindBackend & { readonly rows: Map<string, string> } {
  const rows = new Map<string, string>();
  return {
    rows,
    load: (k) => rows.get(k) ?? null,
    save: (k, v) => void rows.set(k, v),
    remove: (k) => void rows.delete(k),
    keys: () => [...rows.keys()],
  };
}

const G = "group-a";
let now = 1_000_000;
let backend: ReturnType<typeof memoryBackend>;
let store: ConversationBehindStore;

function makeStore(): ConversationBehindStore {
  return new ConversationBehindStore(
    () => wasmBehindVerdict(wasm as unknown as BehindVerdictWasm),
    backend,
    () => "acct",
    () => now,
  );
}

beforeEach(() => {
  now = 1_000_000;
  backend = memoryBackend();
  store = makeStore();
});

const read = (o: Partial<{ ownCommitOwed: boolean; drained: boolean; latestEpoch: bigint | undefined; localEpoch: bigint }>) =>
  store.observeRead(G, {
    ownCommitOwed: o.ownCommitOwed ?? false,
    drained: o.drained ?? true,
    latestEpoch: "latestEpoch" in o ? o.latestEpoch : 7n,
    localEpoch: o.localEpoch ?? 6n,
  });

describe("detection (section 1)", () => {
  it("a drained read with the journal above the local epoch suspects, and the device stops acting", () => {
    const state = read({});
    expect(state.kind).toBe("suspected");
    expect(state.stopsActing).toBe(true);
    expect(store.stopsActing(G)).toBe(true);
    expect(store.isConfirmed(G)).toBe(false);
  });

  it("never suspects with an own commit owed or an undrained read", () => {
    expect(read({ ownCommitOwed: true }).kind).toBe("in-step");
    expect(read({ drained: false }).kind).toBe("in-step");
    expect(read({ latestEpoch: 6n }).kind).toBe("in-step");
    expect(read({ latestEpoch: undefined }).kind).toBe("in-step");
  });

  it("a row framed above the local epoch that failed confirms a suspicion", () => {
    store.noteSkippedRow(G, { sealed: 8n, after: 8n, local: 6n, atMs: 500 });
    const state = read({});
    expect(state.kind).toBe("confirmed");
    expect(store.isConfirmed(G)).toBe(true);
  });

  it("an applied row resets the evidence of a failed row", () => {
    store.noteSkippedRow(G, { sealed: 8n, after: 8n, local: 6n, atMs: 500 });
    store.noteAppliedRow(G);
    expect(read({}).kind).toBe("suspected");
  });

  it("a refused publish confirms at the next suspecting read", () => {
    read({});
    store.notePublishRefused(G);
    expect(store.isConfirmed(G)).toBe(false);
    expect(read({}).kind).toBe("confirmed");
  });

  it("ten minutes of suspicion confirm without other evidence", () => {
    read({});
    now += 10 * 60_000 + 1;
    expect(read({}).kind).toBe("confirmed");
  });

  it("three refused reads suspect and confirm; a skewed clock does not count", () => {
    for (let i = 0; i < 5; i++) store.noteRefusedRead(G, { clockSkewed: true, localEpoch: 6n, ownCommitOwed: false });
    expect(store.state(G).kind).toBe("in-step");
    store.noteRefusedRead(G, { clockSkewed: false, localEpoch: 6n, ownCommitOwed: false });
    store.noteRefusedRead(G, { clockSkewed: false, localEpoch: 6n, ownCommitOwed: false });
    expect(store.state(G).kind).toBe("in-step");
    const third = store.noteRefusedRead(G, { clockSkewed: false, localEpoch: 6n, ownCommitOwed: false });
    expect(third.kind).toBe("confirmed");
  });

  it("a drained read back at the local epoch clears, and the bookkeeping goes", () => {
    store.noteSkippedRow(G, { sealed: 8n, after: 9n, local: 6n, atMs: 500 });
    read({});
    expect(store.isConfirmed(G)).toBe(true);
    expect(read({ latestEpoch: 6n }).kind).toBe("in-step");
    expect(store.gap(G)).toEqual({ gapFromMs: undefined, highestSkippedEpoch: undefined, skippedCommit: false });
    expect(backend.rows.size).toBe(0);
  });

  it("confirmed never goes back to suspected", () => {
    store.noteSkippedRow(G, { sealed: 8n, after: 8n, local: 6n, atMs: 500 });
    read({});
    store.noteAppliedRow(G);
    expect(read({ drained: false }).kind).toBe("confirmed");
  });
});

describe("gap bookkeeping (section 2)", () => {
  it("keeps the first skipped time, the highest epoch and whether a commit was skipped", () => {
    store.noteSkippedRow(G, { sealed: 7n, after: 7n, local: 6n, atMs: 300 });
    store.noteSkippedRow(G, { sealed: 9n, after: 10n, local: 6n, atMs: 400 });
    store.noteSkippedRow(G, { sealed: 8n, after: 8n, local: 6n, atMs: 200 });
    expect(store.gap(G)).toEqual({ gapFromMs: 300, highestSkippedEpoch: 9n, skippedCommit: true });
  });

  it("survives a restart with its state (durable), counters re-learned", () => {
    store.noteSkippedRow(G, { sealed: 8n, after: 8n, local: 6n, atMs: 300 });
    store.observeRead(G, {
      ownCommitOwed: false,
      drained: true,
      latestEpoch: 9n,
      localEpoch: 6n,
      registeredCommits: new Map([
        [5n, 10],
        [7n, 40],
        [8n, 50],
      ]),
    });
    const again = makeStore();
    expect(again.isConfirmed(G)).toBe(true);
    expect(again.gap(G).gapFromMs).toBe(300);
    expect(again.latestEpoch(G)).toBe(9n);
    expect([...again.registeredCommits(G)]).toEqual([
      [7n, 40],
      [8n, 50],
    ]);
    expect(again.behindGroups()).toEqual([G]);
  });

  it("clear() and forgetGroup() drop the durable row", () => {
    read({});
    store.clear(G);
    expect(store.state(G).kind).toBe("in-step");
    expect(backend.rows.size).toBe(0);
    read({});
    store.forgetGroup(G);
    expect(backend.rows.size).toBe(0);
  });
});
