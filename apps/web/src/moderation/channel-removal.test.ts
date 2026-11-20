// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0089 step 5 - a channel moderators removed: the channels fetch gate
 * (a `410 channel-removed` is held, not an end), the verified vs unverified
 * line, the one check per reuse period, the restore, and the channel standing
 * of the standing service (fake core for the cache, the REAL wasm for the
 * prefix and the refusal of an answer that does not check).
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  CHANNEL_RECHECK_FALLBACK_MS,
  CHANNEL_REMOVED,
  ChannelRemovals,
  channelRemovalLine,
  channelRemovalLineKey,
  channelServiceFetch,
  installChannelRemovals,
  isChannelRemovedError,
  nextCheckAt,
} from "./channel-removal";
import { MIN_REUSE_MS, SuspensionMarks, type StandingResult, type StandingWasm } from "./suspension-marks";
import { apiError, fetchPosts, installChannelDeletedHandler, isChannelDeletedError } from "../channels/api";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

afterEach(() => {
  installChannelRemovals(null);
  installChannelDeletedHandler(null);
  vi.unstubAllGlobals();
});

const ID = "ab".repeat(16);
const URL_POSTS = `/api/channels/v1/channels/${ID}/posts?after=0`;

async function settle(): Promise<void> {
  for (let i = 0; i < 5; i += 1) await new Promise((r) => setTimeout(r, 0));
}

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function holder(standing: StandingResult | (() => Promise<StandingResult>), clock = { now: 1_000_000 }) {
  const changes = { n: 0 };
  const h = new ChannelRemovals({
    standing: typeof standing === "function" ? standing : async () => standing,
    now: () => clock.now,
    onChange: () => {
      changes.n += 1;
    },
  });
  return { h, clock, changes };
}

const VERIFIED: StandingResult = { standing: "suspended", recheckAtMs: 1_000_000 + 5 * 60_000 };
const UNKNOWN: StandingResult = { standing: "unknown", recheckAtMs: Number.POSITIVE_INFINITY };

describe("a removed channel (ADR-0089 step 5)", () => {
  it("is a 410 with the channel-removed code, never a deletion", () => {
    expect(isChannelRemovedError(410, CHANNEL_REMOVED)).toBe(true);
    expect(isChannelRemovedError(410, "channel-deleted")).toBe(false);
    expect(isChannelRemovedError(403, CHANNEL_REMOVED)).toBe(false);
    expect(isChannelDeletedError(410, CHANNEL_REMOVED)).toBe(false);
  });

  it("names the catalog line for each state", () => {
    expect(channelRemovalLineKey("removed")).toBe("suspended.channel_removed");
    expect(channelRemovalLineKey("unavailable")).toBe("channels.unavailable");
  });

  it("shows the removal line only once the log verified it", async () => {
    let answer: (r: StandingResult) => void = () => {};
    const { h } = holder(() => new Promise<StandingResult>((r) => (answer = r)));
    const send = vi.fn(async () => json(410, { code: CHANNEL_REMOVED, log_index: 3 }));
    await channelServiceFetch(send, URL_POSTS, undefined, h);
    // Before the standing answers: the neutral line.
    expect(h.line(ID)).toBe("unavailable");
    answer(VERIFIED);
    await settle();
    expect(h.line(ID)).toBe("removed");
  });

  it("says only 'not available' when the removal cannot be checked", async () => {
    for (const standing of [UNKNOWN, { standing: "not" as const, recheckAtMs: 2_000_000 }]) {
      const { h } = holder(standing);
      await channelServiceFetch(async () => json(410, { code: CHANNEL_REMOVED }), URL_POSTS, undefined, h);
      await settle();
      expect(h.line(ID)).toBe("unavailable");
    }
  });

  it("asks the network no more until the check is due, then once", async () => {
    const { h, clock } = holder(VERIFIED);
    const send = vi.fn(async () => json(410, { code: CHANNEL_REMOVED }));
    await channelServiceFetch(send, URL_POSTS, undefined, h);
    await settle();
    expect(send).toHaveBeenCalledTimes(1);

    const held = await channelServiceFetch(send, URL_POSTS, undefined, h);
    expect(held.status).toBe(410);
    expect(((await held.json()) as { code: string }).code).toBe(CHANNEL_REMOVED);
    await channelServiceFetch(send, `/api/channels/v1/channels/${ID}/epoch`, { method: "GET" }, h);
    expect(send).toHaveBeenCalledTimes(1);

    // The snapshot's reuse time passed: exactly one request is the check.
    clock.now = VERIFIED.recheckAtMs;
    let release: () => void = () => {};
    const slow = vi.fn(
      () =>
        new Promise<Response>((r) => {
          release = () => r(json(410, { code: CHANNEL_REMOVED }));
        }),
    );
    const check = channelServiceFetch(slow, URL_POSTS, undefined, h);
    const other = await channelServiceFetch(slow, URL_POSTS, undefined, h);
    expect(other.status).toBe(410);
    expect(slow).toHaveBeenCalledTimes(1);
    release();
    await check;
  });

  it("does not hold another channel or a route that names none", async () => {
    const { h } = holder(VERIFIED);
    await channelServiceFetch(async () => json(410, { code: CHANNEL_REMOVED }), URL_POSTS, undefined, h);
    const send = vi.fn(async () => json(200, {}));
    await channelServiceFetch(send, `/api/channels/v1/channels/${"cd".repeat(16)}/epoch`, undefined, h);
    await channelServiceFetch(send, "/api/channels/v1/directory/bucket/x", undefined, h);
    expect(send).toHaveBeenCalledTimes(2);
  });

  it("returns as before when the check is answered normally (a restore)", async () => {
    const { h, clock } = holder(VERIFIED);
    await channelServiceFetch(async () => json(410, { code: CHANNEL_REMOVED }), URL_POSTS, undefined, h);
    await settle();
    expect(h.line(ID)).toBe("removed");
    clock.now = VERIFIED.recheckAtMs + 1;
    const ok = await channelServiceFetch(async () => json(200, { posts: [] }), URL_POSTS, undefined, h);
    expect(ok.status).toBe(200);
    expect(h.line(ID)).toBeNull();
    // Nothing held: the next request goes out at once.
    const send = vi.fn(async () => json(200, { posts: [] }));
    await channelServiceFetch(send, URL_POSTS, undefined, h);
    expect(send).toHaveBeenCalledTimes(1);
  });

  it("ignores a standing that arrives after the restore", async () => {
    let answer: (r: StandingResult) => void = () => {};
    const { h, clock } = holder(() => new Promise<StandingResult>((r) => (answer = r)));
    await channelServiceFetch(async () => json(410, { code: CHANNEL_REMOVED }), URL_POSTS, undefined, h);
    clock.now += CHANNEL_RECHECK_FALLBACK_MS;
    await channelServiceFetch(async () => json(200, {}), URL_POSTS, undefined, h);
    answer(VERIFIED);
    await settle();
    expect(h.line(ID)).toBeNull();
  });

  it("keeps the removal through a failed check", async () => {
    const { h, clock } = holder(VERIFIED);
    await channelServiceFetch(async () => json(410, { code: CHANNEL_REMOVED }), URL_POSTS, undefined, h);
    await settle();
    clock.now = VERIFIED.recheckAtMs;
    await expect(
      channelServiceFetch(async () => Promise.reject(new TypeError("offline")), URL_POSTS, undefined, h),
    ).rejects.toThrow("offline");
    expect(h.line(ID)).toBe("removed");
    // The failed check put the next one off by the fallback period.
    const send = vi.fn(async () => json(200, {}));
    await channelServiceFetch(send, URL_POSTS, undefined, h);
    expect(send).not.toHaveBeenCalled();
  });

  it("asks again after the reuse period, at least a minute, ten minutes when none", () => {
    expect(nextCheckAt(UNKNOWN, 100)).toBe(100 + CHANNEL_RECHECK_FALLBACK_MS);
    expect(nextCheckAt({ standing: "suspended", recheckAtMs: 150 }, 100)).toBe(100 + MIN_REUSE_MS);
    expect(nextCheckAt({ standing: "suspended", recheckAtMs: 500_000 }, 100)).toBe(500_000);
  });

  it("is not ended by the channels client: the deleted handler never runs, the error carries the code", async () => {
    const { h } = holder(VERIFIED);
    installChannelRemovals(h);
    const deleted = vi.fn();
    installChannelDeletedHandler(deleted);
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => json(410, { code: CHANNEL_REMOVED, detail: "removed", log_index: 9 })),
    );
    await expect(fetchPosts(ID, 0)).rejects.toSatisfy(
      (err: unknown) => apiError.is(err) && err.status === 410 && err.code === CHANNEL_REMOVED,
    );
    await settle();
    expect(deleted).not.toHaveBeenCalled();
    expect(channelRemovalLine(ID)).toBe("removed");
  });
});

describe("the channel standing (ADR-0089 step 5)", () => {
  const LOG = new Uint8Array(32).fill(7);
  const CHANNEL = new Uint8Array(16).fill(5);

  function fakeCore(): StandingWasm & { fetchedPrefixes: string[] } {
    return {
      fetchedPrefixes: [],
      moderationBucketPrefix: () => "0001",
      moderationAccountStanding: () => ({ suspended: false, untilMs: undefined, reuseUntilMs: 0n }),
      moderationChannelBucketPrefix: (id: Uint8Array) => (id[0] ?? 0).toString(16).padStart(4, "0"),
      moderationChannelStanding: (answer: string, id: Uint8Array) => {
        const a = JSON.parse(answer) as { removed: number[]; reuseUntilMs: number };
        const hit = a.removed.includes(id[0] ?? -1);
        return { suspended: hit, untilMs: undefined, reuseUntilMs: BigInt(a.reuseUntilMs), logIndex: hit ? 2n : undefined };
      },
      moderationProveChannelStanding: (answer: string, entry: string, _list: Uint8Array, _root: Uint8Array, id: Uint8Array) => {
        const a = JSON.parse(answer) as { unsigned?: boolean };
        if (a.unsigned === true || entry !== "entry" || id.length !== 16) throw new Error("not-authorised: no");
      },
    };
  }

  /** The proof's deps: a pinned root key, the entry, the list. */
  const PROOF = {
    rootKey: new Uint8Array(32).fill(8),
    fetchEntry: async () => "entry",
    moderatorList: async () => new Uint8Array([1]),
  };

  it("verifies a removed channel through the log and caches per prefix", async () => {
    const fetches: string[] = [];
    const marks = new SuspensionMarks({
      wasm: async () => fakeCore(),
      fetchBucket: async (p) => {
        fetches.push(p);
        return JSON.stringify({ removed: [5], reuseUntilMs: 2_000_000, root: { log_size: 5 } });
      },
      logKey: LOG,
      witnessKeys: [],
      witnessThreshold: 0,
      now: () => 1_000_000,
      ...PROOF,
    });
    expect((await marks.checkChannel(CHANNEL)).standing).toBe("suspended");
    expect((await marks.checkChannel(new Uint8Array(16).fill(6))).standing).toBe("not");
    expect(fetches).toEqual(["0005", "0006"]);
    expect((await marks.checkChannel(CHANNEL)).recheckAtMs).toBe(2_000_000);
    expect(fetches).toHaveLength(2);
  });

  it("is unknown with nothing pinned, and on a core without channel standing", async () => {
    const unpinned = new SuspensionMarks({
      wasm: async () => fakeCore(),
      fetchBucket: async () => "{}",
      logKey: null,
      witnessKeys: [],
      witnessThreshold: 0,
      now: () => 0,
      ...PROOF,
    });
    expect(await unpinned.checkChannel(CHANNEL)).toEqual({ standing: "unknown", recheckAtMs: Number.POSITIVE_INFINITY });
    const old = { moderationBucketPrefix: () => "0001", moderationAccountStanding: fakeCore().moderationAccountStanding };
    const noChannel = new SuspensionMarks({
      wasm: async () => old,
      fetchBucket: async () => "{}",
      logKey: LOG,
      witnessKeys: [],
      witnessThreshold: 0,
      now: () => 0,
      ...PROOF,
    });
    expect((await noChannel.checkChannel(CHANNEL)).standing).toBe("unknown");
  });

  it("shows the removed line only once a moderator's signature is proven (F2)", async () => {
    const proofs: string[] = [];
    const unsigned = new SuspensionMarks({
      wasm: async () => fakeCore(),
      fetchBucket: async () =>
        JSON.stringify({ removed: [5], reuseUntilMs: 2_000_000, root: { log_size: 5 }, unsigned: true }),
      logKey: LOG,
      witnessKeys: [],
      witnessThreshold: 0,
      now: () => 1_000_000,
      ...PROOF,
      fetchEntry: async (index, size) => {
        proofs.push(`${index}/${size}`);
        return "entry";
      },
    });
    expect((await unsigned.checkChannel(CHANNEL)).standing).toBe("unknown");
    expect(proofs).toEqual(["2/5"]);
    const noRoot = new SuspensionMarks({
      wasm: async () => fakeCore(),
      fetchBucket: async () => JSON.stringify({ removed: [5], reuseUntilMs: 2_000_000, root: { log_size: 5 } }),
      logKey: LOG,
      witnessKeys: [],
      witnessThreshold: 0,
      now: () => 1_000_000,
      ...PROOF,
      rootKey: null,
    });
    expect((await noRoot.checkChannel(CHANNEL)).standing).toBe("unknown");

    // The holder then says the neutral line, and still withholds writes.
    const removals = new ChannelRemovals({
      standing: (id) => unsigned.checkChannel(id),
      now: () => 1_000_000,
      onChange: () => {},
    });
    const removedId = "05".repeat(16);
    removals.noteRemoved(removedId);
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));
    expect(removals.line(removedId)).toBe("unavailable");
    expect(removals.mayRequest(removedId)).toBe(false);
    // The same removal, proven, says the removed line.
    const proven = new ChannelRemovals({
      standing: (id) =>
        new SuspensionMarks({
          wasm: async () => fakeCore(),
          fetchBucket: async () => JSON.stringify({ removed: [5], reuseUntilMs: 2_000_000, root: { log_size: 5 } }),
          logKey: LOG,
          witnessKeys: [],
          witnessThreshold: 0,
          now: () => 1_000_000,
          ...PROOF,
        }).checkChannel(id),
      now: () => 1_000_000,
      onChange: () => {},
    });
    proven.noteRemoved(removedId);
    for (let i = 0; i < 5; i += 1) await new Promise((r) => setTimeout(r, 0));
    expect(proven.line(removedId)).toBe("removed");
  });

  it("asks the real core for the channel's bucket and refuses an answer that does not check", async () => {
    const real = wasm as unknown as StandingWasm;
    expect(real.moderationChannelBucketPrefix?.(CHANNEL)).toMatch(/^[0-9a-f]{4}$/);
    const marks = new SuspensionMarks({
      wasm: async () => real,
      fetchBucket: async () => JSON.stringify({ not: "a snapshot" }),
      logKey: LOG,
      witnessKeys: [],
      witnessThreshold: 0,
      now: () => 1_000_000,
      ...PROOF,
    });
    expect((await marks.checkChannel(CHANNEL)).standing).toBe("unknown");
  });
});
