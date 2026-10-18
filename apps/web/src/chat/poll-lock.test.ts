// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * G-38 residual 1, the LOCAL sub-case — `ChatClient.pollMessages` applies a
 * batch under the same per-group MLS lock the four `ContactsClient`
 * create-commit regions take.
 *
 * The hazard is the ADR-0040 one seen from the receive side.
 * `processIncomingVerifyingRoles` mutates the SAME in-memory OpenMLS provider
 * the create-commit paths eager-merge into, so a poll applying a batch while a
 * local commit is parked in its publish interleaves two writers on one group's
 * state. `contacts/commit-lock.test.ts` pins the commit-vs-commit half; this
 * file pins the poll-vs-commit half and the cursor race that comes with it.
 *
 * ## Scope — what these do NOT claim
 *
 * Only the LOCAL interleave (our own poll against our own commit). A poll
 * applying a REMOTE commit while a local one is in flight to the DS is a
 * genuine MLS epoch fork that no client-side lock addresses — it needs a
 * DS-side epoch gate or proposal-based commits. That stays open as G-38
 * residual 1.
 *
 * ## Faithful doubles
 *
 * The mock group's `processIncomingVerifyingRoles` SUCCEEDS and advances the
 * epoch, because a real applied commit does exactly that. A double that threw
 * would send the real loop down its `catch { continue }` arm, and every
 * ordering assertion below would hold with the lock removed — vacuously green.
 * It returns `{ kind: "Commit" }`, which is the one `toEvent` arm that reaches
 * no wasm at all, so the real apply loop runs end to end with no wasm stub in
 * the way.
 *
 * ## Which of these actually discriminate (measured by running the break, and
 * quoted verbatim — not predicted)
 *
 * Neutering `withGroupMlsLock` to a bare `return task()`:
 *   - "does not apply while a commit on the SAME group is parked in its publish"
 *     FAILS with `expected [ 'create:g1', 'persist', …(5) ] to deeply equal
 *     [ 'create:g1', 'persist', …(2) ]`, the diff showing `+ "apply:g#1"`
 *     landing BETWEEN `fetch` and `publish` — the poll merged a row into a
 *     group whose commit was still on the wire, which is the defect itself;
 *   - "a queued poll DROPS a batch the winner already consumed" FAILS with
 *     `expected 'applied' to be 'cursor-moved'`.
 * Re-keying the lock to a single global name:
 *   - "DIFFERENT groups do not block each other" FAILS with
 *     `Error: DEADLOCK: poll on an unrelated group`, so the per-group keying is
 *     load-bearing rather than incidental.
 *
 * The cursor-advance LOCATION is pinned differently, and deliberately so. A
 * source-edit probe that moved `cursorRef.advance` from inside the region to
 * immediately after the `await` in `pollMessages` did NOT discriminate — one
 * microtask is not enough to lose the race, and a probe that passes proves
 * nothing. The faithful shape is the CALLER writing it, which costs the extra
 * ticks of an async return, so it is pinned by two permanent companions below
 * ("the OLD caller-writes shape …" and "a gate whose ADVANCE is caller-side …")
 * that run unmodified product code and reproduce the double-apply — 6 applies
 * for 3 rows, against 3 with the fix.
 *
 * The remaining cases are guards, not discriminators, and say so where they sit.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeAccessSigner, withCommitRoute, withStagedTwins } from "../ds/access-test-doubles";

// No IndexedDB in the node test env; the store round-trip is covered by
// `commit-outbox.test.ts`.
vi.mock("./commit-outbox", () => ({
  markAcked: async () => {},
  loadUnacked: async () => [],
}));

import { ChatClient } from "./client";
import { ContactsClient } from "../contacts/client";
import type { DsClient } from "../ds/client";
import type { DiscoveryClient } from "../discovery/client";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

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

/** Let every pending microtask (and one macrotask turn) drain. */
function settle(): Promise<void> {
  return new Promise((r) => setTimeout(r, 0));
}

/** Fail loudly instead of hanging the suite when a lock deadlocks. */
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

/**
 * A stand-in for an MLS group that mutates on BOTH sides of the hazard: its
 * remove-commit eager-merges N -> N+1 exactly as the real core does inside
 * `removeMember`, and its `processIncomingVerifyingRoles` advances the epoch
 * exactly as applying a real commit does. The live epoch is observable, so a
 * test can count how many writers have run at a given moment — which is
 * precisely what the lock controls.
 */
function statefulGroup(
  groupId: Uint8Array,
  startEpoch: bigint,
  order: string[],
  label: string,
): { group: WasmGroup; epoch: () => bigint } {
  let epoch = startEpoch;
  const group = withStagedTwins({
    get groupId(): Uint8Array {
      return groupId;
    },
    get epoch(): bigint {
      return epoch;
    },
    dsAccessSigner: fakeAccessSigner(() => epoch),
    removeMember: vi.fn((_id: unknown, leaf: number) => {
      epoch += 1n; // the eager merge
      order.push(`create:${label}${leaf}`);
      return new Uint8Array([1, 2, 3]);
    }),
    processIncomingVerifyingRoles: vi.fn((_id: unknown, payload: Uint8Array) => {
      epoch += 1n; // applying a commit advances the epoch, same as the real core
      order.push(`apply:${label}#${payload[0]}`);
      // The `Commit` arm of `toEvent` is the one that reaches no wasm, so the
      // real apply loop runs end to end without a wasm stub in the way.
      return { kind: "Commit" };
    }),
  }) as unknown as WasmGroup;
  return { group, epoch: () => epoch };
}

/** DS rows `1..n`, each payload tagged with its own id so the order log can
 *  name which row was applied. */
function rows(n: number): { messages: unknown[]; hasMore: boolean } {
  return {
    messages: Array.from({ length: n }, (_, i) => ({
      id: i + 1,
      payload: new Uint8Array([i + 1]),
      createdAt: "t",
    })),
    hasMore: false,
  };
}

afterEach(() => {
  vi.clearAllMocks();
});

/**
 * A group id no other test in this file uses.
 *
 * `withGroupMlsLock`'s queue is MODULE-level (deliberately — see its docs), so
 * it outlives an individual test. A shared id would let a test that fails while
 * holding the lock wedge every later test on the same id, which is a property of
 * the harness rather than of the code under test. Found the hard way.
 */
let nextGid = 0;
function freshGid(): Uint8Array {
  nextGid += 1;
  return new Uint8Array([0xa0, nextGid]);
}

describe("G-38 — the poll's apply half is serialized against commits", () => {

  it("does not apply while a commit on the SAME group is parked in its publish", async () => {
    const order: string[] = [];
    const g = statefulGroup(freshGid(), 5n, order, "g");
    const publishGate = deferred<void>();

    const publishGroup = vi.fn(async () => {
      order.push("publish");
      await publishGate.promise; // an ordinary slow DS round-trip
      return { id: 1, createdAt: "t" };
    });
    const fetchGroup = vi.fn(async () => {
      order.push("fetch");
      return rows(3);
    });

    const contacts = new ContactsClient(
      {} as unknown as DiscoveryClient,
      withCommitRoute({ publishGroup, registerGroupEpoch: async () => {} }) as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersistWithCommit: async () => {
          order.push("persist");
        },
      },
    );
    const chat = new ChatClient(
      { fetchGroup } as unknown as DsClient,
      { identityProvider: () => IDENTITY_TOKEN },
    );

    const commit = contacts.removeMemberFromGroup(null, g.group, 1);
    const poll = chat.pollMessages(null, g.group, 0);

    await settle();

    // THE load-bearing assertion. The commit's eager merge has run and it is
    // parked in `publish`; the poll has FETCHED (the network half is outside
    // the lock, deliberately) but has applied NOTHING.
    // `fetch` lands before `publish`: the commit awaits its ADR-0053 epoch
    // registration and its persist first, and the poll's network half is
    // outside the lock so it proceeds meanwhile — which is the split working
    // as designed. (Since ADR-0053 the registration's await lets the fetch in
    // ahead of the persist too; where exactly it lands is not what this test
    // is about.)
    // ADR-0081: the staged commit is recorded, then sent (registration +
    // row); where the poll's fetch lands among them is not what this test is
    // about.
    expect(order.filter((o) => o !== "fetch")).toEqual(["create:g1", "persist", "publish"]);
    expect(order).toContain("fetch");
    expect(g.epoch()).toBe(6n);
    expect(g.group.processIncomingVerifyingRoles).not.toHaveBeenCalled();

    publishGate.resolve();
    await commit;
    const result = await within(1000, poll, "poll queued behind a commit");

    // The apply ran only after the commit's publish returned.
    expect(order.filter((o) => o !== "fetch")).toEqual([
      "create:g1",
      "persist",
      "publish",
      "apply:g#1",
      "apply:g#2",
      "apply:g#3",
    ]);
    expect(order.indexOf("fetch")).toBeLessThan(order.indexOf("apply:g#1"));
    expect(g.epoch()).toBe(9n);
    expect(result.outcome).toBe("applied");
    expect(result.events).toHaveLength(3);
  });

  it("DIFFERENT groups do not block each other", async () => {
    const order: string[] = [];
    const a = statefulGroup(freshGid(), 5n, order, "a");
    const b = statefulGroup(freshGid(), 5n, order, "b");
    const publishGate = deferred<void>();

    const contacts = new ContactsClient(
      {} as unknown as DiscoveryClient,
      withCommitRoute({
        registerGroupEpoch: async () => {},
        publishGroup: vi.fn(async () => {
          order.push("publish");
          await publishGate.promise;
          return { id: 1, createdAt: "t" };
        }),
      }) as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersistWithCommit: async () => {
          order.push("persist");
        },
      },
    );
    const chat = new ChatClient(
      { fetchGroup: vi.fn(async () => rows(1)) } as unknown as DsClient,
      { identityProvider: () => IDENTITY_TOKEN },
    );

    const commit = contacts.removeMemberFromGroup(null, a.group, 1);
    const poll = chat.pollMessages(null, b.group, 0);

    // Group B's poll runs to completion while group A's commit is still parked.
    const result = await within(1000, poll, "poll on an unrelated group");
    expect(order).toContain("apply:b#1");
    expect(result.outcome).toBe("applied");
    expect(b.epoch()).toBe(6n);

    publishGate.resolve();
    await commit;
  });

  it("a queued poll DROPS a batch the winner already consumed (cursor race)", async () => {
    const order: string[] = [];
    const g = statefulGroup(freshGid(), 5n, order, "g");

    // ONE cursor, shared by both polls — this is `routes/Chat.tsx`'s signal.
    let journalCursor = 0;
    const cursorRef = {
      read: () => journalCursor,
      advance: (to: number) => {
        order.push(`cursor:${to}`);
        journalCursor = to;
      },
    };

    const chat = new ChatClient(
      { fetchGroup: vi.fn(async () => rows(3)) } as unknown as DsClient,
      { identityProvider: () => IDENTITY_TOKEN },
    );

    // Two concurrent refreshes. Both read cursor 0 and both fetch rows 1..3 —
    // the DS is a FIFO journal, so the loser is holding rows the winner is
    // about to consume.
    const first = chat.pollMessages(null, g.group, journalCursor, 0, cursorRef);
    const second = chat.pollMessages(null, g.group, journalCursor, 0, cursorRef);

    const [a, b] = await within(
      1000,
      Promise.all([first, second]),
      "two concurrent polls",
    );

    expect(a.outcome).toBe("applied");
    expect(a.events).toHaveLength(3);

    // The loser dropped its stale batch WHOLE — no apply, no cursor write.
    expect(b.outcome).toBe("cursor-moved");
    expect(b.events).toHaveLength(0);

    // Each row was fed to OpenMLS exactly ONCE, and the epoch moved by exactly
    // three. Re-feeding consumed rows is what the gate exists to stop.
    expect(order).toEqual([
      "apply:g#1",
      "apply:g#2",
      "apply:g#3",
      "cursor:3",
    ]);
    expect(g.epoch()).toBe(8n);

    // And the cursor never wound backward: the loser reports the CURRENT
    // position, so even a caller that ignores `outcome` and writes it back is
    // safe.
    expect(journalCursor).toBe(3);
    expect(b.cursor).toBe(3);
  });

  /**
   * The permanent inverted-order companions. Neither edits the source: both run
   * the REAL `pollMessages` under the shape the fix replaced, so the reason the
   * advance sits inside the region stays executable rather than asserted in
   * prose. If someone ever moves it back out, the second one goes green in a way
   * the first makes obvious.
   */
  it("the OLD caller-writes shape re-feeds rows the winner already consumed", async () => {
    const order: string[] = [];
    const g = statefulGroup(freshGid(), 5n, order, "g");
    let journalCursor = 0;

    const chat = new ChatClient(
      { fetchGroup: vi.fn(async () => rows(3)) } as unknown as DsClient,
      { identityProvider: () => IDENTITY_TOKEN },
    );

    // Exactly what `routes/Chat.tsx` used to do: no cursor handed in, the
    // caller writes `result.cursor` back AFTER awaiting.
    const oldShape = async (): Promise<void> => {
      const r = await chat.pollMessages(null, g.group, journalCursor);
      journalCursor = r.cursor;
    };

    await within(1000, Promise.all([oldShape(), oldShape()]), "old shape");

    // Six applies for three rows. Every row was fed to OpenMLS TWICE — the
    // defect, reproduced against unmodified product code.
    expect(order).toEqual([
      "apply:g#1",
      "apply:g#2",
      "apply:g#3",
      "apply:g#1",
      "apply:g#2",
      "apply:g#3",
    ]);
    expect(g.epoch()).toBe(11n);
  });

  it("a gate whose ADVANCE is caller-side is too late — the region has to own it", async () => {
    const order: string[] = [];
    const g = statefulGroup(freshGid(), 5n, order, "g");
    let journalCursor = 0;

    const chat = new ChatClient(
      { fetchGroup: vi.fn(async () => rows(3)) } as unknown as DsClient,
      { identityProvider: () => IDENTITY_TOKEN },
    );

    // The re-read gate IS wired, but the write is deferred to the caller — the
    // shape a "just add a staleness check" fix would land. `advance` is a no-op
    // and the caller writes after awaiting.
    const halfFixed = async (): Promise<void> => {
      const r = await chat.pollMessages(null, g.group, journalCursor, 0, {
        read: () => journalCursor,
        advance: () => {},
      });
      journalCursor = r.cursor;
    };

    await within(1000, Promise.all([halfFixed(), halfFixed()]), "half-fixed");

    // Still six. The loser reaches its re-read a microtask BEFORE the winner's
    // caller-side write lands, sees the stale 0, and judges its batch fresh.
    // The gate is not bypassed by an attacker — it is simply consulted against
    // state nobody has updated yet (ADR-0044 C-62 sub-invariant 5, same shape).
    expect(order).toEqual([
      "apply:g#1",
      "apply:g#2",
      "apply:g#3",
      "apply:g#1",
      "apply:g#2",
      "apply:g#3",
    ]);
    expect(g.epoch()).toBe(11n);
  });

  it("a caller that passes no cursor keeps the previous contract", async () => {
    const order: string[] = [];
    const g = statefulGroup(freshGid(), 5n, order, "g");
    const chat = new ChatClient(
      { fetchGroup: vi.fn(async () => rows(2)) } as unknown as DsClient,
      { identityProvider: () => IDENTITY_TOKEN },
    );

    // Guard, not a discriminator: `pollMessages` still applies and still
    // reports the end cursor for a caller that owns the position itself.
    const r = await chat.pollMessages(null, g.group, 0);
    expect(r.outcome).toBe("applied");
    expect(r.cursor).toBe(2);
    expect(order).toEqual(["apply:g#1", "apply:g#2"]);
  });

  it("the `void refresh()` tail of a commit handler QUEUES, it does not deadlock", async () => {
    const order: string[] = [];
    const g = statefulGroup(freshGid(), 5n, order, "g");

    const contacts = new ContactsClient(
      {} as unknown as DiscoveryClient,
      withCommitRoute({
        registerGroupEpoch: async () => {},
        publishGroup: vi.fn(async () => {
          order.push("publish");
          return { id: 1, createdAt: "t" };
        }),
      }) as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersistWithCommit: async () => {},
      },
    );
    const chat = new ChatClient(
      { fetchGroup: vi.fn(async () => rows(1)) } as unknown as DsClient,
      { identityProvider: () => IDENTITY_TOKEN },
    );

    // Exactly `routes/Chat.tsx`'s shape: the commit is awaited (so its region
    // is left), THEN the poll is launched fire-and-forget. The lock is
    // non-reentrant, so a poll started from INSIDE the region would hang here.
    // Guard, not a discriminator — it pins the audit result rather than a fix.
    let poll!: Promise<unknown>;
    await contacts.removeMemberFromGroup(null, g.group, 1);
    poll = chat.pollMessages(null, g.group, 0);

    await within(1000, poll, "void refresh() after a commit");
    expect(order).toEqual(["create:g1", "publish", "apply:g#1"]);
  });
});
