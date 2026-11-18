// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

vi.mock("../storage/account-kv", () => ({
  loadAccountJson: vi.fn(async () => null),
  saveAccountJson: vi.fn(async () => undefined),
}));

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import { bindContentAccount, updateContent, withEntry, withOp, EMPTY_CONTENT } from "./content-store";
import { latestTicks, latestVotes, openEntries, planHandoverRows } from "./handover";
import { checklistBodyOf } from "./actions";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const groupId = new Uint8Array(32).fill(4);
const authorSeed = new Uint8Array(32).fill(7);
const authorKeyB64 = (): string => base64Encode(wasm.PollVoterKey.fromEd25519Seed(authorSeed).publicKey);

describe("the open-items hand-over planner (ADR-0090 section 3.9)", () => {
  it("keeps each voter's latest vote and drops retracts", () => {
    const v = latestVotes([
      { messageId: 1, createdAtMs: 1, voter: "B", choices: [0] },
      { messageId: 3, createdAtMs: 3, voter: "A", choices: [] },
      { messageId: 2, createdAtMs: 2, voter: "A", choices: [1] },
      { messageId: 4, createdAtMs: 4, voter: "B", choices: [1] },
    ]);
    expect(v).toEqual([{ messageId: 4, createdAtMs: 4, voter: "B", choices: [1] }]);
  });

  it("keeps one tick per item, its latest, and only done ones", () => {
    const t = latestTicks([
      { messageId: 1, ticker: "A", privileged: false, item: { kind: "author", id: 1 }, done: true },
      { messageId: 2, ticker: "A", privileged: false, item: { kind: "author", id: 1 }, done: false },
      { messageId: 3, ticker: "B", privileged: false, item: { kind: "author", id: 2 }, done: true },
    ]);
    expect(t.map((x) => x.messageId)).toEqual([3]);
  });

  it("hands over only open, verified items", () => {
    const body = checklistBodyOf({ title: "T", othersAdd: false, othersMark: true, items: [{ text: "a" }] });
    const signed = wasm.signChecklist(body, groupId, authorSeed);
    let c = withEntry(EMPTY_CONTENT, {
      kind: "checklist",
      messageId: 10,
      createdAtMs: 0,
      authorKeyB64: authorKeyB64(),
      signed,
      verified: true,
      edits: [],
      adds: [],
      ticks: [],
      closes: [],
    });
    c = withEntry(c, {
      kind: "checklist",
      messageId: 11,
      createdAtMs: 0,
      authorKeyB64: authorKeyB64(),
      signed,
      verified: false,
      edits: [],
      adds: [],
      ticks: [],
      closes: [],
    });
    expect(openEntries(c, 0).map((e) => e.messageId)).toEqual([10]);
    c = withOp(c, { kind: "close", target: 10, record: { messageId: 12, closer: authorKeyB64(), closerIsAdmin: false } });
    expect(openEntries(c, 0)).toEqual([]);
  });

  it("packs an open checklist into rows only when a leaf was added", async () => {
    bindContentAccount("acct");
    const body = checklistBodyOf({ title: "Shopping", othersAdd: true, othersMark: true, items: [{ text: "milk" }, { text: "bread" }] });
    const signed = wasm.signChecklist(body, groupId, authorSeed);
    await updateContent("g1", (c) =>
      withEntry(c, {
        kind: "checklist",
        messageId: 20,
        createdAtMs: 0,
        authorKeyB64: authorKeyB64(),
        signed,
        verified: true,
        edits: [],
        adds: [],
        ticks: [{ messageId: 21, ticker: authorKeyB64(), privileged: false, item: { kind: "author", id: 1 }, done: true }],
        closes: [],
      }),
    );
    expect(await planHandoverRows("g1", 0, Date.now())).toEqual([]);
    const rows = await planHandoverRows("g1", 1, Date.now());
    expect(rows.length).toBeGreaterThan(0);
    for (const r of rows) expect(wasm.isOpenItemsEnvelope(r)).toBe(true);
    // The receiving side assembles and adopts what was packed.
    const assembled = wasm.assembleOpenItems(
      rows.map((r) => new wasm.ReceivedOpenItemsRow(new Uint8Array(32).fill(1), r)),
      new BigInt64Array(0),
    );
    expect(assembled.length).toBe(1);
    const adopted = wasm.adoptOpenItem(groupId, assembled[0]!);
    expect((adopted.item as { kind: string; checklist: { body: { title: string } } }).checklist.body.title).toBe("Shopping");
    bindContentAccount(null);
  });
});
