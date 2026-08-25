// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The SEND path is serialized against the regions that move the group's epoch.
 *
 * ## The hazard, stated precisely
 *
 * It is NOT "two `createApplicationMessage` calls interleave". They cannot:
 * the wasm call is synchronous and JS is single-threaded, so no second caller
 * observes the core's mid-call state (including the ADR-0045 pending-proposal
 * park). And it is NOT "two application messages are published out of order" —
 * within one epoch MLS orders by generation, not by journal position, so a
 * reordered pair of same-epoch messages both decrypt.
 *
 * The hazard is that an application message can be SEALED at one epoch and
 * PUBLISHED on the far side of a commit that changes it. `lakko-mls` never
 * calls `MlsGroupCreateConfig::max_past_epochs`, so it is openmls's default of
 * **0** ("If `max_past_epochs` is 0, only the current epoch is being stored",
 * `openmls-0.8.1/src/group/mls_group/past_secrets.rs:44`). A peer therefore
 * keeps NO past-epoch secrets: a straddling message is not late, it is
 * **permanently undecryptable for every recipient** — dropped silently, with
 * the sender's own optimistic echo still on screen.
 *
 * Both directions are reachable, and the send path reaches them because it
 * holds no lock while the four `ContactsClient` commit regions, the poll's
 * apply and the ADR-0045 sweep all do:
 *
 *   - **sealed AHEAD.** A commit region eager-merges N -> N+1 *inside* its
 *     create, then parks in `persist` + `publish`. A send firing in that window
 *     seals at N+1 and can reach the DS first, so peers still at N see a
 *     future-epoch message and drop it.
 *   - **sealed BEHIND.** An ADR-0045 sweep STAGES (the group stays at N) and
 *     publishes the commit, then spends a second round-trip reading the journal
 *     verdict before merging. `MlsGroup::create_message` gates on `is_active()`,
 *     not `is_operational()` (`application.rs:22`), so a pending commit does not
 *     block a send: it seals at N and, the commit already being on the wire,
 *     necessarily publishes after it. In that window the loss is not a race but
 *     a CERTAINTY, for the full duration of the verdict read.
 *
 * ## The oracle
 *
 * {@link replayAsPeer} is the assertion, not a hand-written expectation of call
 * order: it replays the recorded journal in DS id order exactly as a recipient
 * does, at `max_past_epochs = 0`, and reports what that recipient never sees.
 * A test that merely pinned an ordering would keep passing if the ordering
 * changed for a good reason; this one only passes when the journal is
 * *applyable*, which is the property that actually matters.
 *
 * ## Which of these discriminate (measured by running the break, quoted
 * verbatim — not predicted)
 *
 * Removing `withGroupMlsLock` from `broadcastControl` and `sendMessage`:
 *   - "a broadcast does not seal ahead of a commit still parked in its publish"
 *     FAILS with `expected [ 'app@6' ] to deeply equal []`;
 *   - "a broadcast does not seal behind a staged sweep's published commit"
 *     FAILS with `expected [ 'app@5' ] to deeply equal []`;
 *   - "a send does not seal ahead of a commit parked in its publish" FAILS with
 *     `expected [ 'app@6' ] to deeply equal []`;
 *   - "concurrent sends do not overlap their whole-provider persists" FAILS with
 *     `expected [ 'persist:enter', 'persist:enter', …(2) ] to deeply equal
 *     [ 'persist:enter', 'persist:exit', …(2) ]`.
 * The per-group keying is pinned by "an unrelated group's broadcast is not
 * blocked", which fails with `DEADLOCK` if the lock is re-keyed globally.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("./commit-outbox", () => ({
  markAcked: async () => {},
  loadUnacked: async () => [],
}));

import { ChatClient } from "./client";
import { ContactsClient } from "../contacts/client";
import { withGroupMlsLock } from "../mls/group-lock";
import { base64Encode } from "../auth/account-id";
import type { DsClient } from "../ds/client";
import type { DiscoveryClient } from "../discovery/client";
import type { PushClient } from "../push/client";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

/** Wire tags for the group double's two artifact kinds. Byte 1 carries the
 *  epoch the artifact was built on, which is the whole point of the oracle. */
const TAG_APP = 0xa0;
const TAG_COMMIT = 0xc0;

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
 * A stand-in for an MLS group that STAMPS every artifact with the epoch it was
 * built on, so the journal can be replayed the way a recipient replays it.
 *
 * `removeMember` eager-merges N -> N+1 inside the create, exactly as the real
 * core does; `createApplicationMessage` seals at the live epoch and does NOT
 * move it, exactly as `MlsGroup::create_message` does. `stage`/`merge` model
 * the ADR-0045 staged pair: staging leaves the epoch alone, merging advances it.
 */
function stampingGroup(
  groupId: Uint8Array,
  startEpoch: number,
): {
  group: WasmGroup;
  epoch: () => number;
  stage: () => Uint8Array;
  merge: () => void;
} {
  let epoch = startEpoch;
  const group = {
    get groupId(): Uint8Array {
      return groupId;
    },
    get epoch(): bigint {
      return BigInt(epoch);
    },
    removeMember: vi.fn((_id: unknown, _leaf: number) => {
      const builtOn = epoch;
      epoch += 1; // the eager merge, inside the create
      return new Uint8Array([TAG_COMMIT, builtOn]);
    }),
    createApplicationMessage: vi.fn((_id: unknown, _plaintext: Uint8Array) => {
      // Seals at the live epoch and leaves it where it is — the real
      // `create_message` advances the sender ratchet, never the epoch.
      return new Uint8Array([TAG_APP, epoch]);
    }),
  } as unknown as WasmGroup;
  return {
    group,
    epoch: () => epoch,
    // The ADR-0045 staged pair: the commit exists and is publishable while the
    // group deliberately stays at N until the journal verdict is read.
    stage: () => new Uint8Array([TAG_COMMIT, epoch]),
    merge: () => {
      epoch += 1;
    },
  };
}

/** A DS double that keeps the journal in ARRIVAL order, which is the order the
 *  real DS assigns ids in and therefore the order every peer replays. */
function journalDs(): { ds: DsClient; journal: Uint8Array[] } {
  const journal: Uint8Array[] = [];
  const ds = {
    publishGroup: vi.fn(async (_gid: Uint8Array, bytes: Uint8Array) => {
      journal.push(bytes);
      return { id: journal.length, createdAt: "t" };
    }),
  } as unknown as DsClient;
  return { ds, journal };
}

/**
 * Replay `journal` in DS id order the way a recipient does, at
 * `max_past_epochs = 0`, and return the labels of the rows that recipient never
 * sees. A commit applies only when it was built on the epoch the peer is
 * currently at; an application message decrypts only at exactly that epoch —
 * no past secrets are retained and nothing buffers a future epoch.
 */
function replayAsPeer(journal: readonly Uint8Array[], startEpoch: number): string[] {
  let peerEpoch = startEpoch;
  const dropped: string[] = [];
  for (const row of journal) {
    const [tag, builtOn] = [row[0], row[1]];
    if (tag === TAG_COMMIT) {
      if (builtOn === peerEpoch) peerEpoch += 1;
      else dropped.push(`commit@${builtOn}`);
    } else {
      if (builtOn !== peerEpoch) dropped.push(`app@${builtOn}`);
    }
  }
  return dropped;
}

function mockPush(): PushClient {
  return { notifyGroup: vi.fn(async () => {}) } as unknown as PushClient;
}

/** A group id no other case in this file reuses — `withGroupMlsLock`'s queue is
 *  MODULE-level (deliberately), so a shared id would let a case that fails
 *  while holding the lock wedge every later one. */
let nextGid = 0;
function freshGid(): Uint8Array {
  nextGid += 1;
  return new Uint8Array([0xb0, nextGid]);
}

afterEach(() => {
  vi.clearAllMocks();
});

describe("the send path never straddles an epoch change", () => {
  it("a broadcast does not seal ahead of a commit still parked in its publish", async () => {
    const g = stampingGroup(freshGid(), 5);
    const { ds, journal } = journalDs();
    const publishGate = deferred<void>();

    const contacts = new ContactsClient(
      {} as unknown as DiscoveryClient,
      {
        publishGroup: vi.fn(async (_gid: Uint8Array, bytes: Uint8Array) => {
          // A slow DS round-trip: the commit is built and eager-merged, but has
          // not reached the journal yet.
          await publishGate.promise;
          journal.push(bytes);
          return { id: journal.length, createdAt: "t" };
        }),
      } as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersistWithCommit: async () => {},
      },
    );
    const chat = new ChatClient(ds, { identityProvider: () => IDENTITY_TOKEN }, mockPush());

    const commit = contacts.removeMemberFromGroup(null, g.group, 1);
    await settle();
    // The eager merge has already run: the group is at 6 while the commit that
    // takes peers there is still on the wire.
    expect(g.epoch()).toBe(6);

    const beacon = chat.broadcastControl(null, g.group, new Uint8Array([1]));
    await settle();

    publishGate.resolve();
    await commit;
    await within(1000, beacon, "broadcast queued behind a commit");

    expect(replayAsPeer(journal, 5)).toEqual([]);
  });

  it("a broadcast does not seal behind a staged sweep's published commit", async () => {
    // The ADR-0045 sweep shape: stage (epoch stays N), publish the commit, then
    // spend a round-trip on the journal verdict before merging. A send in that
    // window seals at N with the commit already on the wire, so the loss is a
    // certainty rather than a race.
    const g = stampingGroup(freshGid(), 5);
    const { ds, journal } = journalDs();
    const verdictGate = deferred<void>();
    const chat = new ChatClient(ds, { identityProvider: () => IDENTITY_TOKEN }, mockPush());

    const sweep = withGroupMlsLock(base64Encode(g.group.groupId), async () => {
      journal.push(g.stage());
      await verdictGate.promise; // the verdict read
      g.merge();
    });

    await settle();
    const beacon = chat.broadcastControl(null, g.group, new Uint8Array([1]));
    await settle();

    verdictGate.resolve();
    await sweep;
    await within(1000, beacon, "broadcast queued behind a staged sweep");

    expect(replayAsPeer(journal, 5)).toEqual([]);
  });

  it("a send does not seal ahead of a commit parked in its publish", async () => {
    // Same hazard, the higher-severity payload: a user's chat message, which
    // `broadcastControl` and `sendMessage` reach through identical code.
    const g = stampingGroup(freshGid(), 5);
    const { ds, journal } = journalDs();
    const publishGate = deferred<void>();

    const contacts = new ContactsClient(
      {} as unknown as DiscoveryClient,
      {
        publishGroup: vi.fn(async (_gid: Uint8Array, bytes: Uint8Array) => {
          await publishGate.promise;
          journal.push(bytes);
          return { id: journal.length, createdAt: "t" };
        }),
      } as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersistWithCommit: async () => {},
      },
    );
    const chat = new ChatClient(ds, { identityProvider: () => IDENTITY_TOKEN }, mockPush());

    const commit = contacts.removeMemberFromGroup(null, g.group, 1);
    await settle();
    const send = chat.sendMessage(null, g.group, new Uint8Array([7]));
    await settle();

    publishGate.resolve();
    await commit;
    await within(1000, send, "send queued behind a commit");

    expect(replayAsPeer(journal, 5)).toEqual([]);
  });

  it("concurrent sends do not overlap their whole-provider persists", async () => {
    // `ChatContext.persistSnapshot` captures `Identity.snapshotStorage()` — the
    // WHOLE provider — and only then awaits `encryptSnapshot` + `openMlsDb`
    // before creating its IndexedDB transaction. Two overlapping persists are
    // therefore ordered by which of those two awaits settles first, not by
    // which state is newer, so an inverted pair writes the OLDER snapshot last
    // and durably winds the sender ratchet back. Serializing the send region
    // makes the overlap unreachable rather than merely unlikely.
    const g = stampingGroup(freshGid(), 5);
    const { ds } = journalDs();
    const order: string[] = [];
    const gate = deferred<void>();
    let first = true;

    const chat = new ChatClient(
      ds,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersist: async () => {
          order.push("persist:enter");
          if (first) {
            first = false;
            await gate.promise;
          }
          order.push("persist:exit");
        },
      },
      mockPush(),
    );

    const a = chat.sendMessage(null, g.group, new Uint8Array([1]));
    await settle();
    const b = chat.sendMessage(null, g.group, new Uint8Array([2]));
    await settle();

    gate.resolve();
    await within(1000, Promise.all([a, b]), "two sends on one group");

    expect(order).toEqual([
      "persist:enter",
      "persist:exit",
      "persist:enter",
      "persist:exit",
    ]);
  });

  it("an unrelated group's broadcast is not blocked", async () => {
    // Guards the per-group keying: a global key would deadlock this.
    const busy = stampingGroup(freshGid(), 5);
    const other = stampingGroup(freshGid(), 5);
    const { ds } = journalDs();
    const gate = deferred<void>();
    const chat = new ChatClient(ds, { identityProvider: () => IDENTITY_TOKEN }, mockPush());

    const held = withGroupMlsLock(base64Encode(busy.group.groupId), async () => {
      await gate.promise;
    });

    await within(
      1000,
      chat.broadcastControl(null, other.group, new Uint8Array([1])),
      "broadcast on an unrelated group",
    );

    gate.resolve();
    await held;
  });
});
