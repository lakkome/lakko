// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The ongoing-group-call record — the contract shared with the iOS and
 * Android clients (see the module header).
 */
import { describe, expect, it } from "vitest";

import {
  boundStartedAt,
  deserializeGroupCalls,
  GROUP_CALL_EMPTY_GRACE_MS,
  GROUP_CALL_MAX_AGE_MS,
  GROUP_CALL_MAX_RECORDS,
  GROUP_CALL_START_SKEW_MS,
  isGroupCallLive,
  NO_GROUP_CALLS,
  pruneGroupCalls,
  reduceGroupCalls,
  selectLiveGroupCall,
  serializeGroupCalls,
  type GroupCallEvent,
  type GroupCallRecords,
} from "./group-call-record";

const T0 = Date.parse("2026-09-13T12:00:00.000Z");
const A = "aa".repeat(16);
const B = "bb".repeat(16);

function start(callIdHex: string, at = T0, createdAtMs = at): GroupCallEvent {
  return {
    kind: "start",
    callIdHex,
    initiatorLeaf: 1,
    createdAtMs,
    mediaAudio: true,
    mediaVideo: false,
    observedAtMs: at,
  };
}

function join(callIdHex: string, leafIndex: number, at = T0): GroupCallEvent {
  return { kind: "join", callIdHex, leafIndex, observedAtMs: at };
}

function leave(callIdHex: string, leafIndex: number, at = T0): GroupCallEvent {
  return { kind: "leave", callIdHex, leafIndex, observedAtMs: at };
}

function fold(...events: GroupCallEvent[]): GroupCallRecords {
  return events.reduce(reduceGroupCalls, NO_GROUP_CALLS);
}

describe("reduceGroupCalls", () => {
  it("start creates a record with nobody joined", () => {
    const r = fold(start(A)).get(A);
    expect(r?.initiatorLeaf).toBe(1);
    expect(r?.joinedLeaves.size).toBe(0);
    expect(r?.startedAtMs).toBe(T0);
  });

  it("join and leave track the roster of the matching call", () => {
    const r = fold(start(A), join(A, 1), join(A, 4), leave(A, 1)).get(A);
    expect([...(r?.joinedLeaves ?? [])]).toEqual([4]);
  });

  it("end clears the record", () => {
    expect(fold(start(A), join(A, 1), { kind: "end", callIdHex: A }).has(A)).toBe(
      false,
    );
  });

  it("drops a join / leave for a call it never saw start (C-12)", () => {
    const records = fold(start(A), join(A, 1));
    expect(reduceGroupCalls(records, join(B, 9))).toBe(records);
    expect(reduceGroupCalls(records, leave(B, 1))).toBe(records);
    expect(records.get(A)?.joinedLeaves.has(9)).toBe(false);
    expect(records.has(B)).toBe(false);
  });

  it("a repeated start does not reset the roster", () => {
    const records = fold(start(A), join(A, 2));
    expect(reduceGroupCalls(records, start(A, T0 + 5000))).toBe(records);
    expect(records.get(A)?.joinedLeaves.has(2)).toBe(true);
  });

  it("an end for an unknown call changes nothing", () => {
    const records = fold(start(A));
    expect(reduceGroupCalls(records, { kind: "end", callIdHex: B })).toBe(records);
  });

  it("keeps at most the newest records", () => {
    const events: GroupCallEvent[] = [];
    for (let i = 0; i < GROUP_CALL_MAX_RECORDS + 4; i += 1) {
      events.push(start(i.toString(16).padStart(32, "0"), T0 + i * 1000));
    }
    const records = fold(...events);
    expect(records.size).toBe(GROUP_CALL_MAX_RECORDS);
    expect(records.has("0".repeat(32))).toBe(false);
  });
});

describe("liveness", () => {
  it("a call nobody has joined is live only for the grace after it started", () => {
    const r = fold(start(A)).get(A);
    if (r === undefined) throw new Error("no record");
    expect(isGroupCallLive(r, T0 + GROUP_CALL_EMPTY_GRACE_MS)).toBe(true);
    expect(isGroupCallLive(r, T0 + GROUP_CALL_EMPTY_GRACE_MS + 1)).toBe(false);
  });

  it("a call with somebody in it stays live", () => {
    const r = fold(start(A), join(A, 1)).get(A);
    if (r === undefined) throw new Error("no record");
    expect(isGroupCallLive(r, T0 + 60 * 60 * 1000)).toBe(true);
  });

  it("everybody leaving makes it not live once the grace has passed", () => {
    const r = fold(start(A), join(A, 1), leave(A, 1, T0 + 120_000)).get(A);
    if (r === undefined) throw new Error("no record");
    expect(isGroupCallLive(r, T0 + 120_000)).toBe(false);
  });

  it("nothing older than the maximum age is live, whoever is joined", () => {
    const r = fold(start(A), join(A, 1)).get(A);
    if (r === undefined) throw new Error("no record");
    expect(isGroupCallLive(r, T0 + GROUP_CALL_MAX_AGE_MS)).toBe(true);
    expect(isGroupCallLive(r, T0 + GROUP_CALL_MAX_AGE_MS + 1)).toBe(false);
  });

  it("prune drops what is past the maximum age", () => {
    const records = fold(start(A), join(A, 1), start(B, T0 + 10_000));
    const pruned = pruneGroupCalls(records, T0 + GROUP_CALL_MAX_AGE_MS + 1);
    expect(pruned.has(A)).toBe(false);
    expect(pruned.has(B)).toBe(true);
    expect(pruneGroupCalls(records, T0)).toBe(records);
  });
});

describe("selectLiveGroupCall", () => {
  it("picks the lowest (created_at_ms, call_id hex) among live calls", () => {
    // B started first by its initiator's clock.
    const records = fold(
      start(A, T0 + 2000, T0 + 2000),
      join(A, 1, T0 + 2000),
      start(B, T0 + 3000, T0 + 1000),
      join(B, 2, T0 + 3000),
    );
    expect(selectLiveGroupCall(records, T0 + 5000)?.callIdHex).toBe(B);
  });

  it("breaks an exact created_at_ms tie on the call id", () => {
    const records = fold(start(B), join(B, 2), start(A), join(A, 1));
    expect(selectLiveGroupCall(records, T0 + 5000)?.callIdHex).toBe(A);
  });

  it("ignores a call that is no longer live when choosing", () => {
    // A would win the tie-break, but nobody is in it and its grace is over.
    const records = fold(start(A, T0, T0), start(B, T0, T0 + 1), join(B, 2));
    expect(
      selectLiveGroupCall(records, T0 + GROUP_CALL_EMPTY_GRACE_MS + 5)?.callIdHex,
    ).toBe(B);
  });

  it("returns null when nothing is live", () => {
    expect(selectLiveGroupCall(NO_GROUP_CALLS, T0)).toBeNull();
    const records = fold(start(A));
    expect(selectLiveGroupCall(records, T0 + GROUP_CALL_EMPTY_GRACE_MS + 1)).toBeNull();
  });
});

describe("boundStartedAt", () => {
  it("believes a claim within the skew before the row timestamp", () => {
    expect(boundStartedAt(T0 - 1000, T0)).toBe(T0 - 1000);
  });

  it("does not let a sender's clock keep a call recent or date it far back", () => {
    expect(boundStartedAt(T0 + 60_000, T0)).toBe(T0);
    expect(boundStartedAt(T0 - GROUP_CALL_START_SKEW_MS - 1, T0)).toBe(T0);
    expect(boundStartedAt(0, T0)).toBe(T0);
    expect(boundStartedAt(Number.NaN, T0)).toBe(T0);
  });
});

describe("serialisation", () => {
  it("round-trips", () => {
    const records = fold(start(A), join(A, 1), join(A, 7));
    const back = deserializeGroupCalls(serializeGroupCalls(records));
    expect(back.get(A)).toEqual(records.get(A));
  });

  it("skips malformed entries and survives garbage", () => {
    expect(deserializeGroupCalls(null).size).toBe(0);
    expect(deserializeGroupCalls("{not json").size).toBe(0);
    expect(deserializeGroupCalls('{"a":1}').size).toBe(0);
    const good = JSON.parse(serializeGroupCalls(fold(start(A)))) as unknown[];
    const mixed = JSON.stringify([
      ...good,
      { callIdHex: "zz", mediaAudio: true },
      { ...(good[0] as object), callIdHex: "abcd" },
    ]);
    const back = deserializeGroupCalls(mixed);
    expect([...back.keys()]).toEqual([A]);
  });
});
