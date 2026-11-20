// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import { checklistRecordsFromSnapshot, checklistStateFromSnapshot, shownTally } from "./snapshot";
import { replacingSnapshot, withBallotRow, type PollEntry } from "./content-store";
import { checklistBodyOf } from "./actions";
import { channelTimeline, CHANNEL_AUTHOR_B64 } from "../channels/channel-timeline";
import type { PostDecode } from "../channels/post-decode";
import { base64Decode } from "../auth/account-id";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

describe("a restored poll (ADR-0092 section 3.11)", () => {
  const base = { counts: [2, 1], voters: 3, closed: false };
  it("shows the bundle's totals alone, never adding a live vote to them", () => {
    // A live vote may already be in the bundle's totals: adding it would
    // count it twice.
    expect(shownTally({ counts: [0, 1], voters: 1, closed: false }, { baseline: base, fromSnapshot: true })).toEqual(base);
    expect(shownTally(null, { baseline: base, fromSnapshot: true })).toEqual(base);
    expect(shownTally({ counts: [0, 0], voters: 0, closed: true }, { baseline: base, fromSnapshot: true })?.closed).toBe(true);
  });
  it("shows the full fold once the hand-over replaced the bundle entry", () => {
    const live = { counts: [1, 0], voters: 1, closed: false };
    expect(shownTally(live, {})).toEqual(live);
  });
});

describe("the hand-over replaces a bundle entry (no vote counted twice)", () => {
  const poll = { body: { id: "AA==", question: "Q", options: ["a", "b"], multiple: false }, authorSig: null };
  const entry = (over: Partial<PollEntry>): PollEntry => ({
    kind: "poll",
    messageId: 10,
    createdAtMs: 0,
    authorKeyB64: "K",
    signed: poll,
    verified: true,
    votes: [],
    closes: [],
    ballots: [],
    ...over,
  });
  it("takes the hand-over's per-voter votes plus the live ones, LWW by id", () => {
    const held = entry({
      fromSnapshot: true,
      baseline: { counts: [5, 5], voters: 10, closed: false },
      votes: [{ messageId: 50, createdAtMs: 0, voter: "B", choices: [1] }],
    });
    const adopted = entry({ votes: [{ messageId: 20, createdAtMs: 0, voter: "B", choices: [0] }, { messageId: 21, createdAtMs: 0, voter: "C", choices: [0] }] });
    const next = replacingSnapshot(held, adopted) as PollEntry;
    expect(next.fromSnapshot).toBeUndefined();
    expect(next.baseline).toBeUndefined();
    expect(next.votes.map((v) => v.messageId).sort()).toEqual([20, 21, 50]);
  });
  it("never replaces an entry the hand-over already filled", () => {
    const held = entry({ votes: [{ messageId: 1, createdAtMs: 0, voter: "A", choices: [0] }] });
    expect(replacingSnapshot(held, entry({}))).toBe(held);
  });
  it("dedups a ballot row that arrives from the hand-over and again from the journal", () => {
    let e = entry({});
    e = withBallotRow(e, { messageId: -1, createdAtMs: 0, rowB64: "ROW", verified: { tag: "t", choices: [0], version: 0 } });
    e = withBallotRow(e, { messageId: 77, createdAtMs: 5, rowB64: "ROW" });
    expect(e.ballots).toHaveLength(1);
    expect(e.ballots[0]?.messageId).toBe(77);
    expect(e.ballots[0]?.verified).toEqual({ tag: "t", choices: [0], version: 0 });
    e = withBallotRow(e, { messageId: -2, createdAtMs: 0, rowB64: "ROW" });
    expect(e.ballots).toHaveLength(1);
  });
});

describe("a restored checklist", () => {
  const groupId = new Uint8Array(32).fill(3);
  const seed = new Uint8Array(32).fill(5);
  const author = (): string => base64Encode(wasm.PollVoterKey.fromEd25519Seed(seed).publicKey);
  const other = (): string => base64Encode(wasm.PollVoterKey.fromEd25519Seed(new Uint8Array(32).fill(6)).publicKey);

  it("folds the bundle's added and done items as the sender saw them", () => {
    const body = checklistBodyOf({ title: "Trip", othersAdd: true, othersMark: true, items: [{ text: "tent" }, { text: "map" }] });
    const snap = {
      kind: "checklist" as const,
      closedBy: null,
      added: [{ messageId: 30, text: "stove" }],
      done: [
        { item: { kind: "author" as const, id: 2 }, by: other() },
        { item: { kind: "added" as const, messageId: 30 }, by: author() },
      ],
    };
    const view = checklistStateFromSnapshot(body, snap);
    expect(view.items.map((i) => [i.text, i.doneBy])).toEqual([
      ["tent", null],
      ["map", other()],
      ["stove", author()],
    ]);
    const r = checklistRecordsFromSnapshot(author(), 10, snap);
    const folded = wasm.foldChecklist(body, base64Decode(author()), 10n, [], r.adds, r.ticks, r.closes);
    expect(folded.items.map((i) => [i.text, i.doneBy ?? null])).toEqual(view.items.map((i) => [i.text, i.doneBy]));
    void groupId;
  });
});

describe("a channel checklist (UX 8.1: publishers tick and add)", () => {
  it("folds the publishers' tick, add, edit and close posts", () => {
    const body = checklistBodyOf({ title: "Plan", othersAdd: false, othersMark: false, items: [{ text: "a" }, { text: "b" }] });
    const content = { kind: "checklist" as const, checklist: { body, authorSig: null } };
    const decoded = new Map<number, PostDecode>([
      [1, { kind: "content", body: { kind: "content", content, attachments: [] } }],
      [2, { kind: "content-op", body: { kind: "content-op", op: { op: "checklist-tick", target: 1, item: { kind: "author", id: 1 }, done: true } } }],
      [3, { kind: "content-op", body: { kind: "content-op", op: { op: "checklist-add", target: 1, text: "c" } } }],
    ]);
    const page = [1, 2, 3].map((cursor) => ({ cursor, serverTsMs: cursor * 1000 }));
    const post = channelTimeline(page as never, decoded).posts[0]!;
    expect(post.checklistOps?.ticks).toHaveLength(1);
    const ops = post.checklistOps!;
    const folded = wasm.foldChecklist(body, base64Decode(CHANNEL_AUTHOR_B64), 1n, ops.edits, ops.adds, ops.ticks, ops.closes);
    expect(folded.items.map((i) => [i.text, i.doneBy != null])).toEqual([
      ["a", true],
      ["b", false],
      ["c", false],
    ]);
  });
});
