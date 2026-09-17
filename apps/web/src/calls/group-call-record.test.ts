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
  freshMemberKeys,
  freshParticipantCount,
  GROUP_CALL_EMPTY_GRACE_MS,
  GROUP_CALL_HEARTBEAT_MS,
  GROUP_CALL_MAX_AGE_MS,
  GROUP_CALL_MAX_RECORDS,
  GROUP_CALL_MEMBER_FRESH_MS,
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
import { resolveLeafSignatureKey, type RosterMember } from "../chat/author-identity";

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

/** A member is named by its signature key; `person` defaults to the device's
 *  own key (one device per person). */
function join(callIdHex: string, member: string, at = T0, person = member): GroupCallEvent {
  return { kind: "join", callIdHex, memberKey: member, personKey: person, observedAtMs: at };
}

function leave(callIdHex: string, member: string, at = T0): GroupCallEvent {
  return { kind: "leave", callIdHex, memberKey: member, observedAtMs: at };
}

function fold(...events: GroupCallEvent[]): GroupCallRecords {
  return events.reduce(reduceGroupCalls, NO_GROUP_CALLS);
}

describe("reduceGroupCalls", () => {
  it("start creates a record with nobody joined", () => {
    const r = fold(start(A)).get(A);
    expect(r?.initiatorLeaf).toBe(1);
    expect(r?.members.size).toBe(0);
    expect(r?.startedAtMs).toBe(T0);
  });

  it("join and leave track the roster of the matching call", () => {
    const r = fold(start(A), join(A, "k1"), join(A, "k4"), leave(A, "k1")).get(A);
    expect([...(r?.members.keys() ?? [])]).toEqual(["k4"]);
  });

  it("end clears the record", () => {
    expect(fold(start(A), join(A, "k1"), { kind: "end", callIdHex: A }).has(A)).toBe(
      false,
    );
  });

  it("drops a join / leave for a call it never saw start (C-12)", () => {
    const records = fold(start(A), join(A, "k1"));
    expect(reduceGroupCalls(records, join(B, "k9"))).toBe(records);
    expect(reduceGroupCalls(records, leave(B, "k1"))).toBe(records);
    expect(records.get(A)?.members.has("k9")).toBe(false);
    expect(records.has(B)).toBe(false);
  });

  it("a repeated start does not reset the roster", () => {
    const records = fold(start(A), join(A, "k2"));
    expect(reduceGroupCalls(records, start(A, T0 + 5000))).toBe(records);
    expect(records.get(A)?.members.has("k2")).toBe(true);
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

  it("a call whose member keeps its heartbeat stays live", () => {
    const beats: GroupCallEvent[] = [start(A)];
    for (let t = 0; t <= 60 * 60 * 1000; t += GROUP_CALL_HEARTBEAT_MS) beats.push(join(A, "k1", T0 + t));
    const r = fold(...beats).get(A);
    if (r === undefined) throw new Error("no record");
    expect(isGroupCallLive(r, T0 + 60 * 60 * 1000 + 30_000)).toBe(true);
    expect(freshParticipantCount(r, T0 + 60 * 60 * 1000 + 30_000)).toBe(1);
  });

  it("everybody leaving makes it not live once the grace has passed", () => {
    const r = fold(start(A), join(A, "k1"), leave(A, "k1", T0 + 120_000)).get(A);
    if (r === undefined) throw new Error("no record");
    expect(isGroupCallLive(r, T0 + 120_000)).toBe(false);
  });

  it("nothing older than the maximum age is live, whoever is joined", () => {
    const r = fold(start(A), join(A, "k1", T0 + GROUP_CALL_MAX_AGE_MS - 1000)).get(A);
    if (r === undefined) throw new Error("no record");
    expect(isGroupCallLive(r, T0 + GROUP_CALL_MAX_AGE_MS)).toBe(true);
    expect(isGroupCallLive(r, T0 + GROUP_CALL_MAX_AGE_MS + 1)).toBe(false);
  });

  it("prune drops what is past the maximum age", () => {
    const records = fold(start(A), join(A, "k1"), start(B, T0 + 10_000));
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
      join(A, "k1", T0 + 2000),
      start(B, T0 + 3000, T0 + 1000),
      join(B, "k2", T0 + 3000),
    );
    expect(selectLiveGroupCall(records, T0 + 5000)?.callIdHex).toBe(B);
  });

  it("breaks an exact created_at_ms tie on the call id", () => {
    const records = fold(start(B), join(B, "k2"), start(A), join(A, "k1"));
    expect(selectLiveGroupCall(records, T0 + 5000)?.callIdHex).toBe(A);
  });

  it("ignores a call that is no longer live when choosing", () => {
    // A would win the tie-break, but nobody is in it and its grace is over.
    const records = fold(start(A, T0, T0), start(B, T0, T0 + 1), join(B, "k2"));
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
    const records = fold(start(A), join(A, "k1"), join(A, "k7"));
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

describe("heartbeat and freshness", () => {
  it("the freshness window is two heartbeats plus a 30 s poll", () => {
    expect(GROUP_CALL_HEARTBEAT_MS).toBe(60_000);
    expect(GROUP_CALL_MEMBER_FRESH_MS).toBe(150_000);
  });

  it("a member counts only while its newest join is fresh", () => {
    const r = fold(start(A), join(A, "k1", T0 + 1000)).get(A);
    if (r === undefined) throw new Error("no record");
    expect(freshMemberKeys(r, T0 + 1000 + GROUP_CALL_MEMBER_FRESH_MS)).toEqual(new Set(["k1"]));
    expect(freshMemberKeys(r, T0 + 1000 + GROUP_CALL_MEMBER_FRESH_MS + 1).size).toBe(0);
    expect(freshParticipantCount(r, T0 + 1000 + GROUP_CALL_MEMBER_FRESH_MS + 1)).toBe(0);
  });

  it("a heartbeat advances lastSeen, and an older row never lowers it", () => {
    let records = fold(start(A), join(A, "k1", T0 + 1000), join(A, "k1", T0 + 61_000));
    expect(records.get(A)?.members.get("k1")?.lastSeenMs).toBe(T0 + 61_000);
    const before = records;
    records = reduceGroupCalls(records, join(A, "k1", T0 + 30_000));
    expect(records.get(A)?.members.get("k1")?.lastSeenMs).toBe(T0 + 61_000);
    // Nothing moved at all: same instance.
    expect(records).toBe(before);
  });

  it("a participant that died without a Leave stops keeping the call live (the ghost)", () => {
    const r = fold(start(A), join(A, "k1", T0 + 2000)).get(A);
    if (r === undefined) throw new Error("no record");
    // No new rows: only time passes.
    expect(isGroupCallLive(r, T0 + 2000 + GROUP_CALL_MEMBER_FRESH_MS)).toBe(true);
    expect(isGroupCallLive(r, T0 + 2000 + GROUP_CALL_MEMBER_FRESH_MS + 1)).toBe(false);
    expect(selectLiveGroupCall(new Map([[A, r]]), T0 + 3 * 60 * 60 * 1000)).toBeNull();
  });

  it("one fresh member keeps the call live and is the only one counted", () => {
    const r = fold(
      start(A),
      join(A, "ghost", T0 + 1000),
      join(A, "k2", T0 + 1000),
      join(A, "k2", T0 + 200_000),
    ).get(A);
    if (r === undefined) throw new Error("no record");
    expect(isGroupCallLive(r, T0 + 250_000)).toBe(true);
    expect(freshMemberKeys(r, T0 + 250_000)).toEqual(new Set(["k2"]));
    expect(freshParticipantCount(r, T0 + 250_000)).toBe(1);
  });

  it("a leave removes only that member; a rejoin adds it back", () => {
    let records = fold(start(A), join(A, "k1"), join(A, "k2"), leave(A, "k1", T0 + 5000));
    expect([...(records.get(A)?.members.keys() ?? [])]).toEqual(["k2"]);
    records = reduceGroupCalls(records, join(A, "k1", T0 + 9000));
    expect(records.get(A)?.members.get("k1")?.lastSeenMs).toBe(T0 + 9000);
  });

  it("a heartbeat after end does not resurrect the call (C-12)", () => {
    const records = fold(start(A), join(A, "k1"), { kind: "end", callIdHex: A });
    expect(reduceGroupCalls(records, join(A, "k1", T0 + 60_000))).toBe(records);
  });

  it("an empty member key is dropped", () => {
    const records = fold(start(A));
    expect(reduceGroupCalls(records, join(A, ""))).toBe(records);
  });

  it("two devices of one person count as one person, and one leaving keeps the other", () => {
    let records = fold(start(A), join(A, "phone", T0, "alice"), join(A, "laptop", T0, "alice"));
    const r = records.get(A);
    if (r === undefined) throw new Error("no record");
    expect(freshParticipantCount(r, T0)).toBe(1);
    records = reduceGroupCalls(records, leave(A, "phone", T0 + 1000));
    expect([...(records.get(A)?.members.keys() ?? [])]).toEqual(["laptop"]);
  });

  it("serialisation rejects a record whose members are malformed", () => {
    const good = JSON.parse(serializeGroupCalls(fold(start(A), join(A, "k1")))) as Record<
      string,
      unknown
    >[];
    const bad = JSON.stringify([{ ...good[0], members: [["k1", { lastSeenMs: "x" }]] }]);
    expect(deserializeGroupCalls(bad).size).toBe(0);
    const legacy = JSON.stringify([{ ...good[0], members: undefined, joinedLeaves: [1] }]);
    expect(deserializeGroupCalls(legacy).size).toBe(0);
  });
});

describe("receipt-time resolution (C-68)", () => {
  const bob = new Uint8Array(32).fill(0xb0);
  const dave = new Uint8Array(32).fill(0xda);
  const before: RosterMember[] = [
    { leafIndex: 0, signaturePublicKey: new Uint8Array(32).fill(0xa1) },
    { leafIndex: 3, signaturePublicKey: bob },
  ];
  // `remove Bob, add Dave` hands Dave the vacated leaf 3.
  const after: RosterMember[] = [
    { leafIndex: 0, signaturePublicKey: new Uint8Array(32).fill(0xa1) },
    { leafIndex: 3, signaturePublicKey: dave },
  ];

  it("a body from the reused leaf is filed under the newcomer, not the departed member", () => {
    const bobKey = resolveLeafSignatureKey(before, 3);
    if (bobKey === undefined) throw new Error("unresolved");
    let records = fold(start(A), join(A, bobKey, T0 + 1000));
    // Dave's Leave arrives from leaf 3 after the commit: it cannot remove Bob.
    const daveKey = resolveLeafSignatureKey(after, 3);
    if (daveKey === undefined) throw new Error("unresolved");
    expect(daveKey).not.toBe(bobKey);
    records = reduceGroupCalls(records, leave(A, daveKey, T0 + 2000));
    expect(records.get(A)?.members.has(bobKey)).toBe(true);
    // Dave never joined, so he is not in the call; Bob's entry just ages out.
    const r = records.get(A);
    if (r === undefined) throw new Error("no record");
    expect(freshMemberKeys(r, T0 + 3000).has(daveKey)).toBe(false);
    expect(freshMemberKeys(r, T0 + 1000 + GROUP_CALL_MEMBER_FRESH_MS + 1).size).toBe(0);
  });

  it("an unknown leaf does not resolve (the host drops the body)", () => {
    expect(resolveLeafSignatureKey(after, 9)).toBeUndefined();
    expect(resolveLeafSignatureKey([], 3)).toBeUndefined();
  });
});

describe("this device's own entry", () => {
  const OWN = "own-device";
  const notInCall = { ownMemberKey: OWN, inCallIdHex: null };

  it("a stale own session after a reload is not someone in the call", () => {
    // This device joined, then its page reloaded; its Join is still fresh.
    const records = fold(start(A), join(A, OWN, T0 + 1000, "me"));
    const r = records.get(A);
    if (r === undefined) throw new Error("no record");
    const now = T0 + 90_000; // past the empty grace, inside the freshness window
    expect(freshParticipantCount(r, now, notInCall)).toBe(0);
    expect(isGroupCallLive(r, now, notInCall)).toBe(false);
    expect(selectLiveGroupCall(records, now, notInCall)).toBeNull();
    // Without a viewer (or while in a different call) it would have counted.
    expect(freshParticipantCount(r, now)).toBe(1);
    expect(isGroupCallLive(r, now, { ownMemberKey: OWN, inCallIdHex: B })).toBe(false);
  });

  it("while this device is in the call its own entry counts", () => {
    const r = fold(start(A), join(A, OWN, T0 + 1000, "me")).get(A);
    if (r === undefined) throw new Error("no record");
    const inCall = { ownMemberKey: OWN, inCallIdHex: A };
    expect(freshParticipantCount(r, T0 + 90_000, inCall)).toBe(1);
    expect(isGroupCallLive(r, T0 + 90_000, inCall)).toBe(true);
  });

  it("only the device key is ignored: the same person's other device still counts", () => {
    const r = fold(
      start(A),
      join(A, OWN, T0 + 1000, "me"),
      join(A, "my-laptop", T0 + 1000, "me"),
    ).get(A);
    if (r === undefined) throw new Error("no record");
    expect(freshMemberKeys(r, T0 + 90_000, notInCall)).toEqual(new Set(["my-laptop"]));
    expect(freshParticipantCount(r, T0 + 90_000, notInCall)).toBe(1);
    expect(isGroupCallLive(r, T0 + 90_000, notInCall)).toBe(true);
  });
});
