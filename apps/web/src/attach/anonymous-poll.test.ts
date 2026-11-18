// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0092 section 3 end to end on this client's wiring, against the real
 * core: a member signs an anonymous poll over the roster, every receiver
 * files it with version 0 of its ring, two members cast ballots, the rows
 * are filed by the receive path and the fold counts each account once.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

vi.mock("../storage/account-kv", () => ({
  loadAccountJson: vi.fn(async () => null),
  saveAccountJson: vi.fn(async () => undefined),
}));

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import { bindContentAccount, entryOf, loadContent } from "./content-store";
import { receiveBallotRow, receiveCommit, receiveContent, type ReceiveGroup } from "./receive";
import { pollContentOf } from "./actions";
import { castBallotWith } from "./ballot-cast";
import { base64Decode } from "../auth/account-id";
import { backfillBallots } from "./ballot-backfill";
import { updateContent, withBallotRow } from "./content-store";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const groupId = new Uint8Array(32).fill(9);
const seeds = [1, 2, 3, 4].map((n) => new Uint8Array(32).fill(n));
const keyOf = (seed: Uint8Array): Uint8Array => wasm.PollVoterKey.fromEd25519Seed(seed).publicKey;

function groupOf(seedList: Uint8Array[]): ReceiveGroup {
  return {
    groupId,
    members: () => seedList.map((s, i) => ({ leafIndex: i, signaturePublicKey: keyOf(s), accountKey: keyOf(s) })),
    groupKind: () => wasm.GroupKind.Group,
  };
}

describe("an anonymous group poll through the receive path", () => {
  it("counts each voter once, and a revote replaces the earlier ballot", async () => {
    bindContentAccount("acct");
    const g = "anon-g";
    const roster = seeds.slice(0, 3).map((s) => base64Encode(keyOf(s)));
    const content = await pollContentOf(
      { question: "Where?", options: ["A", "B"], multiple: false, anonymous: true },
      groupId,
      seeds[0]!,
      roster,
      Date.now(),
    );
    await receiveContent(groupOf(seeds.slice(0, 3)), g, 100, new Date().toISOString(), roster[0], content);
    await loadContent(g);
    const entry = entryOf(g, 100);
    expect(entry?.kind).toBe("poll");
    if (entry?.kind !== "poll" || content.kind !== "poll") throw new Error("not filed");
    expect(entry.verified).toBe(true);

    const cast = (seed: Uint8Array, choices: number[], setVersion?: number) => {
      const e = entryOf(g, 100);
      if (e?.kind !== "poll" || e.ringB64 === undefined) throw new Error("no ring");
      return castBallotWith(wasm, {
        id: 0,
        groupId,
        pollId: base64Decode(content.poll.body.id),
        secret: base64Decode(content.poll.body.anonymous!.secret),
        ring: base64Decode(e.ringB64),
        voterSeed: seed.slice(),
        choices: Uint8Array.from(choices),
        setVersion,
      });
    };
    const b1 = cast(seeds[1]!, [0]);
    const b2 = cast(seeds[2]!, [1]);
    await receiveBallotRow(groupId, g, 101, new Date().toISOString(), b1.row);
    await receiveBallotRow(groupId, g, 102, new Date().toISOString(), b2.row);
    // Member 1 changes its mind, reusing its recorded set version.
    const b3 = cast(seeds[1]!, [1], b1.setVersion);
    await receiveBallotRow(groupId, g, 103, new Date().toISOString(), b3.row);

    const e = entryOf(g, 100);
    if (e?.kind !== "poll") throw new Error("gone");
    expect(e.ballots.every((b) => b.verified !== null && b.verified !== undefined)).toBe(true);
    const counted = e.ballots.map(
      (b) => new wasm.CountedPollBallot(BigInt(b.messageId), BigInt(b.createdAtMs), base64Decode(b.verified!.tag), Uint8Array.from(b.verified!.choices)),
    );
    const tally = wasm.foldPollBallots(2, false, null, null, counted);
    expect(tally.voters).toBe(2);
    expect(Array.from(tally.counts)).toEqual([0, 2]);
    bindContentAccount(null);
  });

  it("grows the ring when a commit adds an account, and a newcomer can vote", async () => {
    bindContentAccount("acct2");
    const g = "anon-g2";
    const roster = seeds.slice(0, 3).map((s) => base64Encode(keyOf(s)));
    const content = await pollContentOf(
      { question: "Q", options: ["x", "y"], multiple: false, anonymous: true },
      groupId,
      seeds[0]!,
      roster,
      Date.now(),
    );
    await receiveContent(groupOf(seeds.slice(0, 3)), g, 5, new Date().toISOString(), roster[0], content);
    await receiveCommit(groupOf(seeds), g, Date.now());
    const e = entryOf(g, 5);
    if (e?.kind !== "poll" || content.kind !== "poll" || e.ringB64 === undefined) throw new Error("not filed");
    const ring = wasm.PollRing.decode(base64Decode(e.ringB64));
    expect(ring.versionCount).toBe(2);
    expect(ring.contains(keyOf(seeds[3]!))).toBe(true);
    const cast = castBallotWith(wasm, {
      id: 0,
      groupId,
      pollId: base64Decode(content.poll.body.id),
      secret: base64Decode(content.poll.body.anonymous!.secret),
      ring: base64Decode(e.ringB64),
      voterSeed: seeds[3]!.slice(),
      choices: Uint8Array.from([0]),
      setVersion: undefined,
    });
    await receiveBallotRow(groupId, g, 6, new Date().toISOString(), cast.row);
    const after = entryOf(g, 5);
    expect(after?.kind === "poll" && after.ballots[0]?.verified != null).toBe(true);
    bindContentAccount(null);
  });

  it("refuses a poll whose ring does not match its signed hash (fail-closed)", async () => {
    bindContentAccount("acct3");
    const g = "anon-g3";
    const roster = seeds.slice(0, 3).map((s) => base64Encode(keyOf(s)));
    const content = await pollContentOf(
      { question: "Q", options: ["x", "y"], multiple: false, anonymous: true },
      groupId,
      seeds[0]!,
      roster,
      Date.now(),
    );
    // A receiver whose roster differs from the author's.
    await receiveContent(groupOf(seeds.slice(0, 2)), g, 7, new Date().toISOString(), roster[0], content);
    expect(entryOf(g, 7)?.verified).toBe(false);
    bindContentAccount(null);
  });

  it("fetches earlier ballots from the journal, deduped against the hand-over's old rows", async () => {
    bindContentAccount("acct4");
    const g = "anon-g4";
    const roster = seeds.slice(0, 3).map((s) => base64Encode(keyOf(s)));
    const content = await pollContentOf(
      { question: "Q", options: ["x", "y"], multiple: false, anonymous: true },
      groupId,
      seeds[0]!,
      roster,
      Date.now(),
    );
    await receiveContent(groupOf(seeds.slice(0, 3)), g, 40, new Date().toISOString(), roster[0], content);
    const e = entryOf(g, 40);
    if (e?.kind !== "poll" || content.kind !== "poll" || e.ringB64 === undefined) throw new Error("not filed");
    const cast = (seed: Uint8Array, choices: number[]) =>
      castBallotWith(wasm, {
        id: 0,
        groupId,
        pollId: base64Decode(content.poll.body.id),
        secret: base64Decode(content.poll.body.anonymous!.secret),
        ring: base64Decode(e.ringB64!),
        voterSeed: seed.slice(),
        choices: Uint8Array.from(choices),
        setVersion: undefined,
      }).row;
    const r1 = cast(seeds[1]!, [0]);
    const r2 = cast(seeds[2]!, [1]);
    // The hand-over brought r1 as an old row (stand-in id below every real one).
    await updateContent(g, (c) => {
      const p = c.entries["40"];
      if (p?.kind !== "poll") return c;
      return { ...c, entries: { ...c.entries, "40": withBallotRow(p, { messageId: -1, createdAtMs: 0, rowB64: base64Encode(r1) }) } };
    });
    const pages = [
      { messages: [{ id: 41, payload: new Uint8Array([1, 2, 3]), createdAt: "2026-01-01T00:00:00Z" }, { id: 42, payload: r1, createdAt: "2026-01-01T00:00:01Z" }], hasMore: true },
      { messages: [{ id: 43, payload: r2, createdAt: "2026-01-01T00:00:02Z" }], hasMore: false },
    ];
    const asked: number[] = [];
    const seen = await backfillBallots({
      groupId,
      groupIdB64: g,
      pollMessageId: 40,
      page: async (after) => {
        asked.push(after);
        return pages.shift() ?? { messages: [], hasMore: false };
      },
      isBallotRow: (p) => wasm.isPollBallotRow(p),
    });
    expect(asked).toEqual([40, 42]);
    expect(seen).toBe(2);
    const after = entryOf(g, 40);
    if (after?.kind !== "poll") throw new Error("gone");
    expect(after.ballots.map((b) => b.messageId)).toEqual([42, 43]);
    const tally = wasm.foldPollBallots(
      2,
      false,
      null,
      null,
      after.ballots
        .filter((b) => b.verified != null)
        .map((b) => new wasm.CountedPollBallot(BigInt(b.messageId), BigInt(b.createdAtMs), base64Decode(b.verified!.tag), Uint8Array.from(b.verified!.choices))),
    );
    expect(tally.voters).toBe(2);
    expect(Array.from(tally.counts)).toEqual([1, 1]);
    bindContentAccount(null);
  });
});
