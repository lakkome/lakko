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
  chatErrorsHidden,
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

  it("a refused read holds automatic sends until a read is answered (live walk 3)", () => {
    // The walk: two refused reads, then the profile republish and a typing
    // signal went out (still in step) and drew 401s.
    expect(store.holdsAutomaticSends(G)).toBe(false);
    store.noteRefusedRead(G, { clockSkewed: true, localEpoch: 6n, ownCommitOwed: false });
    expect(store.holdsAutomaticSends(G)).toBe(false);
    store.noteRefusedRead(G, { clockSkewed: false, localEpoch: 6n, ownCommitOwed: false });
    expect(store.state(G).kind).toBe("in-step");
    expect(store.holdsAutomaticSends(G)).toBe(true);
    expect(read({ latestEpoch: 6n }).kind).toBe("in-step");
    expect(store.holdsAutomaticSends(G)).toBe(false);
    // Suspected or confirmed holds them too.
    store.noteSkippedRow(G, { sealed: 8n, after: 9n, local: 6n, atMs: 500 });
    read({});
    expect(store.holdsAutomaticSends(G)).toBe(true);
  });

  it("tells its listeners when a conversation is back in step, by a read or by the re-entry", () => {
    const back: string[] = [];
    const off = store.onBackInStep((g) => back.push(g));
    read({});
    expect(store.stopsActing(G)).toBe(true);
    expect(back).toEqual([]);
    read({ latestEpoch: 6n });
    expect(back).toEqual([G]);
    // Already in step: nothing more.
    read({ latestEpoch: 6n });
    expect(back).toEqual([G]);
    read({});
    store.clear(G);
    expect(back).toEqual([G, G]);
    off();
    read({});
    read({ latestEpoch: 6n });
    expect(back).toEqual([G, G]);
  });

  it("a refused-read hold that ends in step re-sends like back-in-step (W3-R6)", () => {
    const back: string[] = [];
    const off = store.onBackInStep((g) => back.push(g));
    // An answered read with no refusal before it: nothing.
    read({ latestEpoch: 6n });
    expect(back).toEqual([]);
    // Refused, then answered in step: the re-send.
    store.noteRefusedRead(G, { clockSkewed: false, localEpoch: 6n, ownCommitOwed: false });
    read({ latestEpoch: 6n });
    expect(back).toEqual([G]);
    // Refused, then an answered read that suspects: no in-step re-send.
    store.noteRefusedRead(G, { clockSkewed: false, localEpoch: 6n, ownCommitOwed: false });
    expect(read({}).kind).toBe("suspected");
    expect(back).toEqual([G]);
    off();
  });

  it("a drained read back at the local epoch clears, and the bookkeeping goes", () => {
    store.noteSkippedRow(G, { sealed: 8n, after: 9n, local: 6n, atMs: 500 });
    read({});
    expect(store.isConfirmed(G)).toBe(true);
    expect(read({ latestEpoch: 6n }).kind).toBe("in-step");
    // Only the read progress stays: the last good read starts a later gap.
    expect(store.gap(G)).toEqual({ gapFromMs: now, highestSkippedEpoch: undefined, skippedCommit: false });
    expect(store.behindGroups()).toEqual([]);
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
    expect(store.gap(G)).toEqual({ gapFromMs: 200, highestSkippedEpoch: 9n, skippedCommit: true });
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

  it("clear() keeps only the join as the last good read; forgetGroup() drops the durable row", () => {
    store.noteAppliedRow(G, 400);
    read({});
    now += 5_000;
    store.clear(G);
    expect(store.state(G).kind).toBe("in-step");
    expect(store.gap(G).gapFromMs).toBe(now);
    expect(makeStore().gap(G).gapFromMs).toBe(now);
    read({});
    store.forgetGroup(G);
    expect(backend.rows.size).toBe(0);
  });
});

describe("a waiting chat shows no error (live walk 2)", () => {
  it("hidden while suspected or confirmed, shown in step", () => {
    expect(chatErrorsHidden(store.state(G))).toBe(false);
    store.noteSkippedRow(G, { sealed: 8n, after: 8n, local: 6n, atMs: 500 });
    read({});
    expect(chatErrorsHidden(store.state(G))).toBe(true);
    expect(chatErrorsHidden({ kind: "suspected", sinceMs: 1, stopsActing: true })).toBe(true);
  });
});

describe("where the gap starts (live walk 2)", () => {
  const refused = () => store.noteRefusedRead(G, { clockSkewed: false, localEpoch: 6n, ownCommitOwed: false });

  it("behind through refused reads: the newest applied row, not the moment of suspicion", () => {
    read({ latestEpoch: 6n, drained: false });
    store.noteAppliedRow(G, 400);
    store.noteAppliedRow(G, 300);
    now += 600_000;
    refused();
    refused();
    expect(refused().kind).toBe("confirmed");
    expect(store.gap(G).gapFromMs).toBe(400);
    // Durable before any request goes out.
    expect(makeStore().gap(G).gapFromMs).toBe(400);
  });

  it("an own publish the DS accepted counts as known", () => {
    store.noteAppliedRow(G, 400);
    store.noteOwnRowAccepted(G, 450);
    expect(store.gap(G).gapFromMs).toBe(450);
  });

  it("with no row applied it is the last good read, written at most once a minute", () => {
    read({ latestEpoch: 6n, drained: false });
    const first = now;
    now += 30_000;
    read({ latestEpoch: 6n, drained: false });
    expect(makeStore().gap(G).gapFromMs).toBe(first);
    now += 31_000;
    read({ latestEpoch: 6n, drained: false });
    expect(makeStore().gap(G).gapFromMs).toBe(now);
  });

  it("an earlier skipped row starts the gap; a later one does not", () => {
    store.noteAppliedRow(G, 400);
    store.noteSkippedRow(G, { sealed: 8n, after: 8n, local: 6n, atMs: 500 });
    expect(store.gap(G).gapFromMs).toBe(400);
  });

  it("the newest known row survives coming back in step", () => {
    store.noteAppliedRow(G, 400);
    store.noteSkippedRow(G, { sealed: 8n, after: 9n, local: 6n, atMs: 500 });
    read({});
    expect(read({ latestEpoch: 6n }).kind).toBe("in-step");
    expect(store.gap(G).gapFromMs).toBe(400);
  });
});
