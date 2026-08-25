// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 S4a obligation 5 — the four membership paths become
 * propose-or-sweep, and a group with no role table keeps direct commits.
 *
 * `chat/sweep.test.ts` pins the sweep ALGORITHM over its ports. This file pins
 * the thing that decides which algorithm runs at all: the branch in
 * `ContactsClient`, driven through the real `withGroupMlsLock` regions against
 * faithful wasm-`Group` doubles.
 *
 * ## Which of these actually discriminate (MEASURED — each probe was run)
 *
 * 1. **`a group with NO role table still direct-commits`** — with
 *    `usesProposalPath` forced to `true`:
 *
 *        ContactsError: group.proposeRemoveMember is not a function
 *
 *    i.e. a legacy group would be routed into a path whose peers' receipt gates
 *    drop every proposal it emits, so nothing it asks for would ever apply.
 *
 * 2. **`a role-table group PROPOSES rather than committing`** — with
 *    `usesProposalPath` forced to `false`. FIVE tests went red; the first:
 *
 *        AssertionError: expected { kind: 'committed', …(2) } to match object
 *        { kind: 'proposed' }
 *        -   "kind": "proposed",
 *        +   "kind": "committed",
 *
 *    That `committed` is the eager merge this whole ADR exists to remove from
 *    the concurrent path.
 *
 * 3. **`the owner's own change still applies immediately`** — with the
 *    `rank !== 0` gate in `runCommitterSweep` changed to reject rank 0. THREE tests
 *    went red:
 *
 *        AssertionError: expected 'not-committer' to be 'committed'
 *
 *    The owner would publish a request nobody ever sweeps, so "tap, done"
 *    becomes an indefinite wait in the overwhelmingly common case.
 *
 * 4. **`a plain member's request waits for a committer`** — with the
 *    `rank === undefined` gate made permissive:
 *
 *        AssertionError: expected { kind: 'committed', …(3) } to deeply equal
 *        { kind: 'not-committer', rank: undefined }
 *
 *    A plain member's commit is refused by every peer's role gate, so it would
 *    eager-merge alone at N+1 — this ADR's own hazard through the back door.
 *
 * 5. **S4b — `an admin sweeps when nobody better placed is running`** and
 *    `an admin defers to a fresh beacon from a lower rank`. The admin arm is
 *    the whole of S4b: an admin-only group used to wait indefinitely.
 *
 * BOUNDARY GUARD, not a discriminator: `the sweep never runs on a legacy
 * group`. It passes under every probe above; its job is to catch a future
 * change that let the sweep reach a group whose core would refuse it.
 */

import { afterEach, describe, expect, it, vi } from "vitest";

// No IndexedDB in the node test env; the store round-trip is covered by
// `src/chat/commit-outbox.test.ts`.
const markAcked = vi.fn(async () => {});
const loadUnacked = vi.fn(async () => [] as unknown[]);
const deleteEntry = vi.fn(async () => {});
vi.mock("../chat/commit-outbox", () => ({
  markAcked: (...a: unknown[]) => markAcked(...(a as [])),
  loadUnacked: (...a: unknown[]) => loadUnacked(...(a as [])),
  deleteEntry: (...a: unknown[]) => deleteEntry(...(a as [])),
}));

vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    IdentityPseudonym: {
      fromIdentity: (pk: Uint8Array) => ({
        asBytes: (): Uint8Array => new Uint8Array([0xbb, pk[0] ?? 0]),
      }),
    },
  }),
}));

import { ContactsClient } from "./client";
import type { ActingCommitter } from "../chat/committer-beacon";
import type { DsClient } from "../ds/client";
import type { DiscoveryClient } from "../discovery/client";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;
const OWNER = new Uint8Array(32).fill(1);

/** A group id no other case reuses — `withGroupMlsLock`'s queue is
 *  module-level, so a shared id would let one failure wedge the next case. */
let nextGid = 0;
function freshGid(): Uint8Array {
  nextGid += 1;
  return new Uint8Array([0xc0, nextGid]);
}

interface Fake {
  group: WasmGroup;
  order: string[];
  epoch: () => bigint;
  pending: () => number;
  staged: () => boolean;
}

/**
 * A faithful MLS group double.
 *
 * The fidelity that matters: `propose*` does NOT advance the epoch and DOES
 * grow the pending set; `stagePendingProposals` does NOT advance the epoch;
 * only `mergeStagedSweep` does, and only a merge empties the pending set. A
 * double that advanced the epoch on a proposal would make every assertion here
 * vacuous, because the code under test reads exactly that.
 */
function fakeGroup(opts: {
  gid: Uint8Array;
  roleTable: boolean;
  rank?: number | undefined;
  pending?: number;
}): Fake {
  const order: string[] = [];
  let epoch = 6n;
  let pending = opts.pending ?? 0;
  let staged = false;
  const base: Record<string, unknown> = {
    get groupId(): Uint8Array {
      return opts.gid;
    },
    get epoch(): bigint {
      return epoch;
    },
    get memberCount(): number {
      return 3;
    },
    pendingProposals: () =>
      Array.from({ length: pending }, () => ({
        kind: "remove",
        senderLeaf: 1,
        stored: true,
        addSignatureKey: undefined,
        removeTargetLeaf: 4,
      })),
    // The direct-commit paths. Eager merge, exactly like the core.
    removeMember: vi.fn(() => {
      order.push("removeMember");
      epoch += 1n;
      return new Uint8Array([1, 2, 3]);
    }),
    addMember: vi.fn(() => {
      order.push("addMember");
      epoch += 1n;
      return { commit: new Uint8Array([1]), welcome: new Uint8Array([2]) };
    }),
  };
  if (opts.roleTable) {
    Object.assign(base, {
      roleTableOwner: () => OWNER,
      roleTableAdmins: () => new Uint8Array(0),
      proposeRemoveMember: vi.fn(() => {
        order.push("proposeRemoveMember");
        pending += 1; // and the epoch deliberately does NOT move
        return new Uint8Array([0xaa]);
      }),
      proposeAddMember: vi.fn(() => {
        order.push("proposeAddMember");
        pending += 1;
        return new Uint8Array([0xab]);
      }),
      ownCommitterRank: () => opts.rank,
      hasStagedSweep: () => staged,
      stagePendingProposals: vi.fn(() => {
        if (pending === 0) return undefined;
        order.push("stage");
        staged = true;
        return {
          commit: new Uint8Array([0x5e]),
          welcome: undefined,
          swept: () => [],
        };
      }),
      mergeStagedSweep: vi.fn(() => {
        order.push("merge");
        staged = false;
        epoch += 1n;
        pending = 0;
      }),
      abandonStagedSweep: vi.fn(() => {
        order.push("abandon");
        staged = false;
      }),
      processIncomingVerifyingRoles: vi.fn(() => {
        order.push("apply");
        return { kind: "Commit" };
      }),
    });
  } else {
    // A legacy group: reading the role table throws, exactly like a real wasm
    // `Group` with no `0xFF02` extension. `ownCommitterRank` is still PRESENT
    // and returns `undefined` — the real method exists on every `Group` — so
    // the short-circuit under test has to be a real decision rather than an
    // accidental TypeError.
    Object.assign(base, {
      roleTableOwner: () => {
        throw new Error("no role table");
      },
      ownCommitterRank: () => undefined,
      hasStagedSweep: () => false,
    });
  }
  return {
    group: base as unknown as WasmGroup,
    order,
    epoch: () => epoch,
    pending: () => pending,
    staged: () => staged,
  };
}

function client(
  order: string[],
  ds: Partial<DsClient>,
  discovery: Partial<DiscoveryClient> = {},
): ContactsClient {
  return new ContactsClient(
    discovery as unknown as DiscoveryClient,
    ds as unknown as DsClient,
    {
      identityProvider: () => IDENTITY_TOKEN,
      onPersist: async () => {
        order.push("persist");
      },
      onPersistWithCommit: async () => {
        order.push("record");
      },
    },
  );
}

function ds(order: string[], journal: unknown[] = []): Partial<DsClient> {
  let id = 100;
  return {
    publishGroup: vi.fn(async () => {
      order.push("publish");
      return { id: (id += 1), createdAt: "t" };
    }),
    publishMailbox: vi.fn(async () => {
      order.push("publish-mailbox");
      return { id: (id += 1), createdAt: "t" };
    }),
    fetchGroup: vi.fn(async () => ({
      order: order.push("fetch"),
      messages: journal,
      hasMore: false,
    })) as unknown as DsClient["fetchGroup"],
  };
}

afterEach(() => {
  vi.clearAllMocks();
});

const CURSOR = { read: (): number => 0, advance: (): void => {} };
const NOOP = (): void => {};
/** ADR-0045 S4b — the acting-committer port. `ACT` is what the real fold gives
 *  the owner unconditionally and gives an admin that has heard nothing; `defer`
 *  is what a fresh beacon from a strictly better-placed candidate gives. */
const ACT = async (): Promise<ActingCommitter> => ({ kind: "act" });
const DEFER_TO = (toRank: number) => async (): Promise<ActingCommitter> => ({
  kind: "defer",
  toRank,
});

describe("ADR-0045 obligation 5 — which path a group takes", () => {
  it("a group with NO role table still direct-commits", async () => {
    const g = fakeGroup({ gid: freshGid(), roleTable: false });
    const c = client(g.order, ds(g.order));

    const out = await c.removeMemberFromGroup(null, g.group, 4);

    expect(out).toMatchObject({ kind: "committed" });
    // create -> record -> publish, the ADR-0040 sequence, byte-for-byte as it
    // was. A legacy group has no owner, hence no committer, and every peer's
    // receipt gate would drop a proposal it emitted — so routing it into the
    // proposal path would mean nothing it asks for ever applies.
    expect(g.order).toEqual(["removeMember", "record", "publish"]);
    expect(g.epoch()).toBe(7n);
  });

  it("a role-table group PROPOSES rather than committing", async () => {
    const g = fakeGroup({ gid: freshGid(), roleTable: true, rank: undefined });
    const c = client(g.order, ds(g.order));

    const out = await c.removeMemberFromGroup(null, g.group, 4);

    expect(out).toMatchObject({ kind: "proposed" });
    expect(g.order).toEqual(["proposeRemoveMember", "persist", "publish"]);
    // The whole point of the option: the epoch did NOT move, so a proposal that
    // loses a race costs nothing, where a commit that loses one forks its
    // author off the group.
    expect(g.epoch()).toBe(6n);
    expect(g.pending()).toBe(1);
    // And NO outbox entry — there is nothing to roll back, so writing one would
    // put a never-committed artifact into the restart sweep's re-publish set.
    expect(g.order).not.toContain("record");
  });

  it("the sweep never runs on a legacy group", async () => {
    const g = fakeGroup({ gid: freshGid(), roleTable: false });
    const c = client(g.order, ds(g.order));

    const out = await c.sweepIfCommitter(null, g.group, CURSOR, NOOP, ACT);

    // `ownCommitterRank` is absent on a legacy group, so the adapter's read
    // throws and the sweep must not proceed regardless.
    expect(out.kind).toBe("not-committer");
    expect(g.order).toEqual([]);
  });
});

describe("ADR-0045 — who sweeps", () => {
  it("the owner's own change still applies immediately", async () => {
    // UX §2 — the load-bearing common case. The owner is rank 0, so its own
    // device is the acting committer and sweeps its own request with no round
    // trip to anyone: the user experience stays "tap, done".
    const g = fakeGroup({ gid: freshGid(), roleTable: true, rank: 0 });
    const c = client(g.order, ds(g.order));

    const proposed = await c.removeMemberFromGroup(null, g.group, 4);
    expect(proposed).toMatchObject({ kind: "proposed" });

    const swept = await c.sweepIfCommitter(null, g.group, CURSOR, NOOP, ACT);

    expect(swept.kind).toBe("committed");
    expect(g.order).toEqual([
      "proposeRemoveMember",
      "persist",
      "publish",
      "fetch", // the stand-down re-read
      "stage",
      "record", // ADR-0040, BEFORE the publish
      "publish",
      "fetch", // the verdict read
      "merge",
      "persist",
    ]);
    expect(g.epoch()).toBe(7n);
    expect(g.pending()).toBe(0);
    expect(g.staged()).toBe(false);
  });

  it("an admin defers to a fresh beacon from a lower rank — it does not sweep", async () => {
    // ADR-0045 S4b. An admin (rank 1) that hears the owner (rank 0) beacon
    // inside the freshness window stands down: only a STRICTLY lower rank
    // defers it, and rank 0 is strictly lower. Nothing is staged, so nothing
    // can race the owner's own sweep.
    const g = fakeGroup({ gid: freshGid(), roleTable: true, rank: 1 });
    const c = client(g.order, ds(g.order));

    await c.removeMemberFromGroup(null, g.group, 4);
    const swept = await c.sweepIfCommitter(
      null,
      g.group,
      CURSOR,
      NOOP,
      DEFER_TO(0),
    );

    expect(swept).toEqual({ kind: "deferred", rank: 1, toRank: 0 });
    expect(g.pending()).toBe(1);
    expect(g.epoch()).toBe(6n);
    expect(g.order).not.toContain("stage");
    // The stand-down happens BEFORE the journal re-read, so a deferring device
    // does not even reach the network.
    expect(g.order).not.toContain("fetch");
  });

  it("an admin sweeps when nobody better placed is running", async () => {
    // ADR-0045 S4b — the whole point of the slice. Under S4a this admin
    // returned `not-committer` and the group waited for an owner who might
    // never open the app. Hearing nothing means ACT.
    const g = fakeGroup({ gid: freshGid(), roleTable: true, rank: 1 });
    const c = client(g.order, ds(g.order));

    await c.removeMemberFromGroup(null, g.group, 4);
    const swept = await c.sweepIfCommitter(null, g.group, CURSOR, NOOP, ACT);

    expect(swept.kind).toBe("committed");
    expect(g.epoch()).toBe(7n);
    expect(g.pending()).toBe(0);
    expect(g.staged()).toBe(false);
    // The stand-down re-read is still there — the beacon removed the WAIT,
    // not the RACE.
    expect(g.order).toContain("fetch");
  });

  it("a plain member's request waits for a committer", async () => {
    const g = fakeGroup({ gid: freshGid(), roleTable: true, rank: undefined });
    const c = client(g.order, ds(g.order));

    await c.removeMemberFromGroup(null, g.group, 4);
    const swept = await c.sweepIfCommitter(null, g.group, CURSOR, NOOP, ACT);

    // `undefined` and `0` are never interchangeable: `0` is the owner.
    expect(swept).toEqual({ kind: "not-committer", rank: undefined });
    expect(g.pending()).toBe(1);
  });

  it("an admin's proposal reaches the owner and the owner sweeps it", async () => {
    // The two devices share nothing but the DS journal, which is modelled here
    // as the admin's published proposal arriving in the owner's pending set —
    // which is what the owner's receipt gate does with it.
    const admin = fakeGroup({ gid: freshGid(), roleTable: true, rank: 1 });
    const adminClient = client(admin.order, ds(admin.order));
    await adminClient.removeMemberFromGroup(null, admin.group, 4);
    expect(admin.pending()).toBe(1);

    // The owner, holding that same proposal.
    const owner = fakeGroup({
      gid: freshGid(),
      roleTable: true,
      rank: 0,
      pending: 1,
    });
    const ownerClient = client(owner.order, ds(owner.order));

    const swept = await ownerClient.sweepIfCommitter(
      null,
      owner.group,
      CURSOR,
      NOOP,
      ACT,
    );

    expect(swept.kind).toBe("committed");
    expect(owner.epoch()).toBe(7n);
    expect(owner.pending()).toBe(0);
  });
});

describe("ADR-0045 — the restart sweep resolves an owed verdict FIRST", () => {
  it("skips a staged-sweep entry in the legacy re-publish loop", async () => {
    // A sweep entry is not re-publishable on the ADR-0040 path: its device is
    // durably at N (not N+1), and its Welcome must not reach the wire until the
    // verdict. Re-publishing it there would publish that Welcome unconditionally
    // — the three-party fork.
    const g = fakeGroup({ gid: freshGid(), roleTable: true, rank: 0 });
    const order: string[] = [];
    loadUnacked.mockResolvedValueOnce([
      {
        id: "sweep-1",
        accountId: "a",
        groupId: "g",
        targetEpoch: "6",
        commitB64: "AQID",
        secondArtifactB64: "BAUG",
        secondArtifactKind: "welcome",
        sweepVerdictPending: true,
        acked: false,
      },
    ]);
    const c = client(order, ds(order));

    // No cursor supplied -> the owed-verdict step is skipped, and the legacy
    // loop must still refuse to touch the sweep entry.
    await c.sweepPendingCommits(g.group, NOOP);

    expect(order).not.toContain("publish");
    expect(order).not.toContain("publish-mailbox");
  });

  it("re-publishes an ordinary direct-commit entry as before", async () => {
    const g = fakeGroup({ gid: freshGid(), roleTable: false });
    const order: string[] = [];
    loadUnacked.mockResolvedValueOnce([
      {
        id: "direct-1",
        accountId: "a",
        groupId: "g",
        targetEpoch: "7",
        commitB64: "AQID",
        acked: false,
      },
    ]);
    const c = client(order, ds(order));

    await c.sweepPendingCommits(g.group, NOOP);

    expect(order).toContain("publish");
  });
});
