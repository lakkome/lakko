// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * G-38 residual 3 — `CoPublishersPanel`'s 5 s interval poll applies MLS
 * messages to a `PublisherGroup` concurrently with its own `approveCoPublisher`,
 * which eager-merges an Add into that same group. Same ADR-0040 interleave as
 * the chat poll (`chat/poll-lock.test.ts`), on a PublisherGroup instead of a
 * chat group, so it takes the same per-group `withGroupMlsLock`.
 *
 * The approve's own `busy()` signal is not that guard: it gates the BUTTON, not
 * the interval, and the interval is a separate owner that never consults it.
 *
 * ## Faithful doubles
 *
 * `processIncomingVerifyingRoles` SUCCEEDS and advances the epoch here, because
 * a real applied commit does. The production code wraps that call in a bare
 * `catch {}`, so a double that threw would make every ordering assertion below
 * hold with the lock removed — vacuously green. Likewise `approveExternalJoin`
 * really does advance the epoch, because that eager merge IS the hazard.
 *
 * ## Which of these actually discriminate (measured by running the break, and
 * quoted verbatim — not predicted)
 *
 * Neutering `withGroupMlsLock` to a bare `return task()`:
 *   - "the interval poll does not apply while an approve is mid-publish" FAILS
 *     with `expected [ 'fetch', 'approve-merge', …(2) ] to deeply equal
 *     [ 'fetch', 'approve-merge', …(1) ]`, the diff showing `+ "apply:#7"` — the
 *     poll merged a journal row into a group whose Add had not landed and whose
 *     subscribers had not been resealed;
 *   - "the reseal reads the epoch the approve committed" FAILS with
 *     `expected [ 8 ] to deeply equal [ 6 ]`, i.e. the seed handed to every
 *     subscriber was read at an epoch two concurrent applies had already moved
 *     past — the co-publisher and the subscribers would derive different seeds.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

const sealSubscriberSeedEnvelope = vi.fn(() => new Uint8Array([0xaa]));
// ADR-0053 — the Welcome goes to the joiner's mailbox, addressed by the
// pseudonym of its signature key.
const IdentityPseudonym = {
  fromIdentity: vi.fn(() => ({ asBytes: () => new Uint8Array(32).fill(0x3d) })),
};

vi.mock("../wasm", () => ({
  loadWasm: async () => ({ sealSubscriberSeedEnvelope, IdentityPseudonym }),
}));

const fetchEpoch = vi.fn(async () => ({ epoch: 1 }));
const listSubscribers = vi.fn(async () => ({ subscribers: ["aabb"] }));
const rotateChannel = vi.fn(async () => {});

vi.mock("./api", () => ({
  fetchEpoch: (...a: unknown[]) => fetchEpoch(...(a as [])),
  listSubscribers: (...a: unknown[]) => listSubscribers(...(a as [])),
  rotateChannel: (...a: unknown[]) => rotateChannel(...(a as [])),
  rememberPublisherRole: async () => {},
}));

import { approveCoPublisher, pollCoPublisherJournal } from "./copublisher";
import { fakeAccessSigner } from "../ds/access-test-doubles";
import type { DsClient } from "../ds/client";

interface Deferred<T> {
  readonly promise: Promise<T>;
  resolve: (v: T) => void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

function settle(): Promise<void> {
  return new Promise((r) => setTimeout(r, 0));
}

async function within<T>(ms: number, p: Promise<T>, what: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout>;
  const bomb = new Promise<never>((_, rej) => {
    timer = setTimeout(() => rej(new Error(`DEADLOCK: ${what}`)), ms);
  });
  try {
    return await Promise.race([p, bomb]);
  } finally {
    clearTimeout(timer!);
  }
}

/** `withGroupMlsLock`'s queue is module-level, so each test gets its own id —
 *  see the same note in `chat/poll-lock.test.ts`. */
let nextGid = 0;
function freshGid(): Uint8Array {
  nextGid += 1;
  return new Uint8Array([0xb0, nextGid]);
}

/** A PublisherGroup that mutates on BOTH sides of the hazard: the approve's
 *  eager merge and the poll's apply each advance the epoch, exactly as the real
 *  core does. `channelSeed` reports the LIVE epoch, so a reseal that read a
 *  stale one is directly observable. */
function statefulPublisherGroup(
  groupId: Uint8Array,
  order: string[],
): { group: unknown; epoch: () => number } {
  let epoch = 5;
  const group = {
    groupId: () => groupId,
    dsAccessSigner: fakeAccessSigner(() => BigInt(epoch)),
    channelId: () => new Uint8Array([7, 7]),
    channelSeed: vi.fn(() => {
      order.push(`seed@${epoch}`);
      return new Uint8Array([epoch]);
    }),
    approveExternalJoin: vi.fn(() => {
      epoch += 1; // the eager merge
      order.push("approve-merge");
      return { commit: new Uint8Array([1]), welcome: new Uint8Array([2]) };
    }),
    processIncomingVerifyingRoles: vi.fn((_id: unknown, p: Uint8Array) => {
      epoch += 1; // applying a commit advances the epoch
      order.push(`apply:#${p[0]}`);
    }),
    pendingExternalJoins: () => new Uint8Array(0),
  };
  return { group, epoch: () => epoch };
}

const IDENTITY = {} as never;

/** Reach the poll-apply spy on a mock PublisherGroup without a cast per use. */
function applyMock(group: unknown): ReturnType<typeof vi.fn> {
  return (group as { processIncomingVerifyingRoles: ReturnType<typeof vi.fn> })
    .processIncomingVerifyingRoles;
}

afterEach(() => {
  vi.clearAllMocks();
});

describe("G-38 residual 3 — the co-publisher poll is serialized against approve", () => {
  it("the interval poll does not apply while an approve is mid-publish", async () => {
    const order: string[] = [];
    const g = statefulPublisherGroup(freshGid(), order);
    const publishGate = deferred<void>();

    let publishes = 0;
    const ds = {
      registerGroupEpoch: vi.fn(async () => {}),
      publishGroup: vi.fn(async () => {
        publishes += 1;
        order.push("publish-commit");
        if (publishes === 1) await publishGate.promise;
        return { id: publishes, createdAt: "t" };
      }),
      // ADR-0053 contract point 8 — the Welcome, to the joiner's mailbox.
      publishMailbox: vi.fn(async () => {
        order.push("publish-welcome");
        return { id: 99, createdAt: "t" };
      }),
      fetchKnocks: vi.fn(async () => ({ messages: [], hasMore: false })),
      fetchGroup: vi.fn(async () => {
        order.push("fetch");
        return {
          messages: [{ id: 7, payload: new Uint8Array([7]), createdAt: "t" }],
          hasMore: false,
        };
      }),
    } as unknown as DsClient;

    const approve = approveCoPublisher(
      ds,
      IDENTITY,
      g.group as never,
      new Uint8Array([3]),
      "beef",
    );
    const poll = pollCoPublisherJournal(ds, IDENTITY, g.group as never);

    await settle();

    // The approve's eager merge has run and it is parked in its commit publish;
    // the poll has FETCHED (network half outside the lock) but applied nothing.
    // `fetch` can land before the approve even acquires — that IS the split
    // working (the network half is outside the lock). The load-bearing part is
    // what is ABSENT: no apply, and the epoch moved by exactly the one Add.
    expect(order).toEqual(["fetch", "approve-merge", "publish-commit"]);
    expect(g.epoch()).toBe(6);
    expect(applyMock(g.group)).not.toHaveBeenCalled();

    publishGate.resolve();
    await within(1000, approve, "approve");
    await within(1000, poll, "co-publisher poll queued behind an approve");

    // The reseal happened INSIDE the region, so it read the epoch the approve
    // committed (6) — not one a concurrent apply had already moved past.
    expect(order).toEqual([
      "fetch",
      "approve-merge",
      "publish-commit",
      "publish-welcome",
      "seed@6",
      "apply:#7",
    ]);
    expect(g.epoch()).toBe(7);
  });

  it("the reseal reads the epoch the approve committed, not a concurrently-applied one", async () => {
    const order: string[] = [];
    const g = statefulPublisherGroup(freshGid(), order);
    const publishGate = deferred<void>();
    const seeds: number[] = [];

    let publishes = 0;
    const ds = {
      registerGroupEpoch: vi.fn(async () => {}),
      publishGroup: vi.fn(async () => {
        publishes += 1;
        if (publishes === 1) await publishGate.promise;
        return { id: publishes, createdAt: "t" };
      }),
      publishMailbox: vi.fn(async () => ({ id: 99, createdAt: "t" })),
      fetchKnocks: vi.fn(async () => ({ messages: [], hasMore: false })),
      fetchGroup: vi.fn(async () => ({
        // Two more commits waiting on the journal — each would advance the
        // epoch out from under an unserialized reseal.
        messages: [
          { id: 8, payload: new Uint8Array([8]), createdAt: "t" },
          { id: 9, payload: new Uint8Array([9]), createdAt: "t" },
        ],
        hasMore: false,
      })),
    } as unknown as DsClient;

    sealSubscriberSeedEnvelope.mockImplementation(
      (..._a: unknown[]) => new Uint8Array([0xaa]),
    );
    (g.group as { channelSeed: ReturnType<typeof vi.fn> }).channelSeed
      .mockImplementation(() => {
        // Keep writing to `order` as well — replacing that was how the first
        // run of this test lost the seed read from the ordering log.
        order.push(`seed@${g.epoch()}`);
        seeds.push(g.epoch());
        return new Uint8Array([g.epoch()]);
      });

    const approve = approveCoPublisher(
      ds,
      IDENTITY,
      g.group as never,
      new Uint8Array([3]),
      "beef",
    );
    const poll = pollCoPublisherJournal(ds, IDENTITY, g.group as never);
    await settle();
    publishGate.resolve();
    await within(1000, Promise.all([approve, poll]), "approve + poll");

    // 5 -> 6 by the Add. The two journal commits apply only AFTER the reseal.
    expect(seeds).toEqual([6]);
    expect(g.epoch()).toBe(8);
    expect(order).toEqual(["approve-merge", "seed@6", "apply:#8", "apply:#9"]);

  });
});
