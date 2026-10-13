// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `syncAll` across a change of account. The channel state it reads is the
 * signed-in account's and is swapped on sign-out; the run outlives that by as
 * long as its fetches take. A run for account A must fold nothing into the
 * state account B now owns, report nothing to B's feed and write no row.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const calls = vi.hoisted(() => ({
  folded: [] as string[],
  saved: [] as string[],
  marks: [] as string[],
  published: [] as string[],
  fetchGate: null as null | { promise: Promise<void>; resolve: () => void },
  fetched: [] as string[],
}));

vi.mock("../auth/AuthContext", () => ({ useAuth: () => ({}) }));
vi.mock("../auth/key-store", () => ({ getDeviceKey: async () => ({}) }));
vi.mock("../wasm", () => ({
  loadWasm: async () => ({ classifyChannelPostPlaintext: () => ({}) }),
}));
vi.mock("../channels/api", () => ({
  fetchPosts: async (id: string) => {
    calls.fetched.push(id);
    await calls.fetchGate?.promise;
    return { posts: [{ cursor: 1, serverTsMs: 1, payloadB64: "p" }] };
  },
}));
vi.mock("../channels/composer", () => ({
  decodeAndDecryptPost: async () => ({ hadSeed: true, epochId: 1 }),
}));
vi.mock("../channels/PostCard", () => ({
  toPostDecode: () => ({ kind: "meta" }),
}));
vi.mock("../channels/channel-meta", () => ({
  foldChannelMeta: async (id: string) => {
    calls.folded.push(id);
  },
}));
vi.mock("../state/channels-state", () => ({
  channelsState: {
    channels: () => [{ id: "aa" }],
    keysByChannel: () => new Map([["aa", { seed: new Uint8Array(32) }]]),
    subscribedSeedsByChannel: () => new Map(),
    hydrated: () => true,
  },
}));
vi.mock("./feed-marks-store", () => ({
  updateFeedMarks: async (account: string) => {
    calls.marks.push(account);
  },
}));
vi.mock("./feed-refresh-store", () => ({ reportChannelCheck: () => undefined }));
vi.mock("./channel-post-log-store", () => ({
  loadChannelPostLog: async () => null,
  saveChannelPostLog: async (account: string) => {
    calls.saved.push(account);
  },
  publishChannelPostLogChanged: (account: string) => {
    calls.published.push(account);
  },
}));

import { syncAll } from "./ChannelPostSync";

function gate(): { promise: Promise<void>; resolve: () => void } {
  let resolve!: () => void;
  const promise = new Promise<void>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

beforeEach(() => {
  calls.folded.length = 0;
  calls.saved.length = 0;
  calls.marks.length = 0;
  calls.published.length = 0;
  calls.fetched.length = 0;
  calls.fetchGate = null;
});

describe("syncAll", () => {
  it("folds the metadata and writes the row while its session lasts", async () => {
    const tally = await syncAll("A");
    expect(tally).toEqual({ tried: 1, answered: 1 });
    expect(calls.folded).toEqual(["aa"]);
    expect(calls.marks).toEqual(["A"]);
    expect(calls.saved).toEqual(["A"]);
    expect(calls.published).toEqual(["A"]);
  });

  it("writes nothing once the account changed while a channel was being fetched", async () => {
    let current = true;
    calls.fetchGate = gate();
    const run = syncAll("A", () => current);
    await vi.waitFor(() => expect(calls.fetched).toEqual(["aa"]));
    // Sign out, sign in as B: the channel state is B's from here on.
    current = false;
    calls.fetchGate.resolve();
    expect(await run).toBeNull();
    expect(calls.folded).toEqual([]);
    expect(calls.marks).toEqual([]);
    expect(calls.saved).toEqual([]);
    expect(calls.published).toEqual([]);
  });

  it("does not start for a session that ended before the device key was read", async () => {
    expect(await syncAll("A", () => false)).toBeNull();
    expect(calls.fetched).toEqual([]);
    expect(calls.saved).toEqual([]);
  });
});
