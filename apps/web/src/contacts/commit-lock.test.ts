// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0040 (G-35) — the per-group serializing lock over the four
 * `ContactsClient` create-commit -> publish regions.
 *
 * The lock itself now lives in `src/mls/group-lock.ts`: G-38 brought the poll's
 * apply half (`chat/client.ts`) and the co-publisher poll + approve
 * (`channels/copublisher.ts`) under the SAME queue, and one resource must have
 * exactly one lock. These tests still drive it through `ContactsClient`, which
 * is the surface that matters here; `chat/poll-lock.test.ts` and
 * `channels/copublisher-lock.test.ts` cover the two new callers.
 *
 * The hazard: the `lakko-mls` create-commit paths **eagerly merge** epoch
 * N -> N+1 into the shared in-memory provider INSIDE the create call, before the
 * caller publishes anything. Two of those regions interleaved on one group both
 * build on N; the second publishes a commit on an epoch peers have not reached,
 * and if it lands first the peers drop it and the committing device is durably
 * ahead of a group that never adopted its last commit — the "device-ahead" shape
 * ADR-0040 exists to prevent. The outbox sweep cannot repair it (the entry was
 * acked, so there is nothing to re-publish).
 *
 * The `busy` signal in `routes/Chat.tsx` is not that guard — it is one shared
 * boolean with several independent owners, and `refresh()` is launched
 * fire-and-forget (`void refresh()`) at the tail of every commit handler, so it
 * outlives the handler that started it and clears the flag out from under an
 * unrelated commit still in flight. The last test in this file reproduces that
 * ownership pattern directly, so the reachability claim is executable rather
 * than asserted in prose.
 *
 * The doubles here are deliberately FAITHFUL rather than convenient: the mock
 * group's create-commit really does advance an epoch, and the role-table mock
 * really does read back what the previous commit wrote. A double that threw (or
 * ignored the write) where the real core mutates would let these tests pass with
 * the lock removed.
 *
 * ## Which of these actually discriminate (measured, not assumed)
 *
 * Neutering `withGroupMlsLock` to a bare `return task()`:
 *   - "a second commit on the SAME group does not create until the first has
 *     published" FAILS with `expected 7n to be 6n` — two eager merges ran while
 *     the first op was still parked in its publish. This is the defect itself;
 *   - "SERIALIZING, not coalescing" FAILS on the commit ids.
 * Re-keying the lock to a single global name instead:
 *   - "DIFFERENT groups do not block each other" TIMES OUT, so the per-group
 *     keying is load-bearing rather than incidental.
 * The remaining cases are guards, not discriminators, and say so where they sit.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

// No IndexedDB in the node test env; the store round-trip is covered by
// `src/chat/commit-outbox.test.ts`.
const markAcked = vi.fn(async () => {});
const loadUnacked = vi.fn(async () => [] as unknown[]);
vi.mock("../chat/commit-outbox", () => ({
  markAcked: (...a: unknown[]) => markAcked(...(a as [])),
  loadUnacked: (...a: unknown[]) => loadUnacked(...(a as [])),
}));

import { ContactsClient } from "./client";
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

/**
 * A stand-in for an MLS group whose remove-commit eager-merges N -> N+1
 * exactly as the real core does inside `removeMember`. The live epoch is
 * observable so a test can count how many creates have run at a given moment —
 * which is precisely what serialization controls.
 */
function statefulGroup(
  groupId: Uint8Array,
  startEpoch: bigint,
  order: string[],
  label: string,
): { group: WasmGroup; epoch: () => bigint } {
  let epoch = startEpoch;
  const group = {
    get groupId(): Uint8Array {
      return groupId;
    },
    get epoch(): bigint {
      return epoch;
    },
    removeMember: vi.fn((_id: unknown, leaf: number) => {
      epoch += 1n; // the eager merge
      order.push(`create:${label}${leaf}`);
      return new Uint8Array([1, 2, 3]);
    }),
  } as unknown as WasmGroup;
  return { group, epoch: () => epoch };
}

afterEach(() => {
  vi.clearAllMocks();
});

describe("ADR-0040 — the commit region is serialized per group", () => {
  const GID = new Uint8Array([9, 9, 9, 9]);

  it("a second commit on the SAME group does not create until the first has published", async () => {
    const order: string[] = [];
    const g = statefulGroup(GID, 5n, order, "g");
    const gate = deferred<void>();
    const targetEpochs: string[] = [];
    let publishes = 0;

    const publishGroup = vi.fn(async () => {
      publishes += 1;
      order.push("publish");
      // The FIRST publish hangs — modelling an ordinary slow DS round-trip,
      // which is the whole window the UI `busy` flag fails to cover.
      if (publishes === 1) await gate.promise;
      return { id: publishes, createdAt: "t" };
    });

    const client = new ContactsClient(
      {} as unknown as DiscoveryClient,
      { publishGroup } as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersistWithCommit: async (e) => {
          order.push("persist");
          targetEpochs.push((e as { targetEpoch: string }).targetEpoch);
        },
      },
    );

    // Two clicks, neither awaited — exactly how the UI dispatches them once the
    // shared `busy` flag has been cleared by an unrelated in-flight `refresh()`.
    const opA = client.removeMemberFromGroup(null, g.group, 1);
    const opB = client.removeMemberFromGroup(null, g.group, 2);

    await settle();

    // The load-bearing assertion. Op A is parked in its publish; op B must not
    // have created ANYTHING yet, so exactly one eager merge has happened.
    expect(g.epoch()).toBe(6n);
    expect(order).toEqual(["create:g1", "persist", "publish"]);

    gate.resolve();
    await Promise.all([opA, opB]);

    // B's create ran only after A's publish returned, so B built on the epoch A
    // actually published — never on an epoch no peer has reached.
    expect(order).toEqual([
      "create:g1",
      "persist",
      "publish",
      "create:g2",
      "persist",
      "publish",
    ]);
    expect(g.epoch()).toBe(7n);
    expect(targetEpochs).toEqual(["6", "7"]);
  });

  it("SERIALIZING, not coalescing — both distinct removals run in full", async () => {
    const order: string[] = [];
    const g = statefulGroup(GID, 5n, order, "g");
    const gate = deferred<void>();
    let publishes = 0;
    const publishGroup = vi.fn(async () => {
      publishes += 1;
      if (publishes === 1) await gate.promise;
      return { id: publishes, createdAt: "t" };
    });

    const client = new ContactsClient(
      {} as unknown as DiscoveryClient,
      { publishGroup } as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersistWithCommit: async () => {},
      },
    );

    const opA = client.removeMemberFromGroup(null, g.group, 1);
    const opB = client.removeMemberFromGroup(null, g.group, 2);
    await settle();
    gate.resolve();
    const [a, b] = await Promise.all([opA, opB]);

    // Two clicks are two intended membership changes. Folding the second into
    // the first would silently drop a removal the operator believes happened —
    // that was `lakko-security` F1 in the ADR-0043 review.
    expect(publishGroup).toHaveBeenCalledTimes(2);
    expect(order).toEqual(["create:g1", "create:g2"]);
    // ADR-0045 — these doubles carry no `roleTableOwner`, so `usesProposalPath`
    // reads them as legacy groups and they keep the direct-commit path.
    expect(a.kind).toBe("committed");
    expect(b.kind).toBe("committed");
    expect(a.kind === "committed" ? a.commitMessageId : -1).toBe(1);
    expect(b.kind === "committed" ? b.commitMessageId : -1).toBe(2);
  });

  it("DIFFERENT groups do not block each other", async () => {
    const order: string[] = [];
    const g1 = statefulGroup(new Uint8Array([1, 1]), 5n, order, "a");
    const g2 = statefulGroup(new Uint8Array([2, 2]), 5n, order, "b");
    const gate = deferred<void>();

    const publishGroup = vi.fn(async (gid: Uint8Array) => {
      // Group 1's publish hangs; group 2 must sail past it.
      if (gid[0] === 1) await gate.promise;
      return { id: 1, createdAt: "t" };
    });

    const client = new ContactsClient(
      {} as unknown as DiscoveryClient,
      { publishGroup } as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersistWithCommit: async () => {},
      },
    );

    const opA = client.removeMemberFromGroup(null, g1.group, 1);
    const opB = client.removeMemberFromGroup(null, g2.group, 1);

    // Group 2 completes while group 1 is still parked in its publish.
    await expect(opB).resolves.toEqual({
      kind: "committed",
      commitMessageId: 1,
      welcomeMessageId: undefined,
    });
    expect(g2.epoch()).toBe(6n);

    gate.resolve();
    await opA;
    expect(g1.epoch()).toBe(6n);
  });

  it("a FAILING predecessor does not poison its successor", async () => {
    const order: string[] = [];
    const g = statefulGroup(GID, 5n, order, "g");
    let persists = 0;
    const publishGroup = vi.fn(async () => ({ id: 7, createdAt: "t" }));

    const client = new ContactsClient(
      {} as unknown as DiscoveryClient,
      { publishGroup } as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersistWithCommit: async () => {
          persists += 1;
          if (persists === 1) throw new Error("IndexedDB quota hiccup");
        },
        // Model the ADR-0040 F1 reset: reload at the last durable epoch.
        onCommitPersistAbort: async () => {
          // The abort rolled the provider back; nothing else to do here.
        },
      },
    );

    const opA = client.removeMemberFromGroup(null, g.group, 1);
    const opB = client.removeMemberFromGroup(null, g.group, 2);

    await expect(opA).rejects.toThrow();
    // The queued op still runs — a predecessor's failure is its own caller's,
    // and B's trigger (a real removal the operator asked for) is unaffected.
    await expect(opB).resolves.toEqual({
      kind: "committed",
      commitMessageId: 7,
      welcomeMessageId: undefined,
    });
    expect(publishGroup).toHaveBeenCalledTimes(1);
  });
});

/**
 * ADR-0045 — what a role change does on the PROPOSAL path, and why the lock
 * stopped being sufficient for it.
 *
 * This suite used to assert that two concurrent promotions BOTH land, because
 * the second read the table the first had COMMITTED. On the proposal path that
 * argument dissolves: a proposal changes nothing until a committer sweeps it,
 * so two toggles taken a moment apart both read the SAME committed table and
 * each proposes a table carrying only its own change. RFC 9420 allows at most
 * one `GroupContextExtensions` proposal per commit, so a sweep keeps one and
 * discards the other — the first promotion would vanish with no error anywhere.
 * Serializing does not help, because there is nothing for the second read to
 * observe; and recomputing from "the committed table plus our own pending
 * proposal" is not available either, since `ProposalKind::RoleChange` carries
 * no payload (the core's `classify` discards the extensions).
 *
 * So `setMemberAdmin` REFUSES a second role change while one is pending, and
 * these tests pin both halves: the refusal, and — the half that matters — that
 * the refusal is not hiding a lost promotion, because the first request is
 * still pending and lands on the next sweep.
 */
describe("ADR-0045 — a second role change is refused while one is pending", () => {
  const GID = new Uint8Array([9, 9, 9, 9]);
  const OWNER = new Uint8Array(32).fill(1);
  const ALICE = new Uint8Array(32).fill(0xa1);
  const BOB = new Uint8Array(32).fill(0xb0);

  /**
   * A faithful role-table group on the ADR-0045 proposal path.
   *
   * The load-bearing fidelity is that `proposeGroupRoles` does NOT touch
   * `adminsFlat` and does NOT advance the epoch — a proposal changes nothing
   * until it is swept, and a double that mutated the table here would make the
   * lost-promotion hazard unreachable and every assertion below vacuous. It
   * accumulates into `pendingProposals()` instead, which is what the real core
   * does and what the refusal reads.
   */
  function roleGroup(): {
    group: WasmGroup;
    admins: () => Uint8Array;
    proposed: () => Uint8Array[];
  } {
    const epoch = 5n;
    const adminsFlat = new Uint8Array(0);
    const pending: Array<{ kind: string }> = [];
    const proposed: Uint8Array[] = [];
    const group = {
      get groupId(): Uint8Array {
        return GID;
      },
      get epoch(): bigint {
        return epoch;
      },
      roleTableOwner: vi.fn(() => OWNER),
      roleTableAdmins: vi.fn(() => adminsFlat),
      pendingProposals: vi.fn(() => [...pending]),
      proposeGroupRoles: vi.fn(
        (_id: unknown, _owner: Uint8Array, next: Uint8Array) => {
          proposed.push(next.slice());
          pending.push({ kind: "role-change" });
          return new Uint8Array([4, 5, 6]);
        },
      ),
    } as unknown as WasmGroup;
    return { group, admins: () => adminsFlat, proposed: () => proposed };
  }

  it("the SECOND concurrent promotion is refused, and the first survives as a pending request", async () => {
    const g = roleGroup();
    const gate = deferred<void>();
    let publishes = 0;
    const publishGroup = vi.fn(async () => {
      publishes += 1;
      if (publishes === 1) await gate.promise;
      return { id: publishes, createdAt: "t" };
    });

    const client = new ContactsClient(
      {} as unknown as DiscoveryClient,
      { publishGroup } as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersistWithCommit: async () => {},
      },
    );

    const opA = client.setMemberAdmin(null, g.group, ALICE, true);
    const opB = client.setMemberAdmin(null, g.group, BOB, true);

    await settle();
    gate.resolve();

    await expect(opA).resolves.toMatchObject({ kind: "proposed" });
    await expect(opB).rejects.toThrow(/already waiting to apply/);

    // Exactly ONE proposal reached the wire, and it is A's — so B's refusal did
    // not cost A's promotion. Without the refusal, B's table (naming only BOB,
    // computed from the same empty committed set) would have been published
    // second and a sweep would have dropped A's silently.
    expect(publishGroup).toHaveBeenCalledTimes(1);
    expect(g.proposed()).toHaveLength(1);
    expect(Array.from(g.proposed()[0] as Uint8Array)).toEqual(Array.from(ALICE));
    // And the committed table has NOT moved — a proposal is not a commit.
    expect(g.admins().length).toBe(0);
  });

  it("a role change is allowed again once nothing is pending", async () => {
    const g = roleGroup();
    const publishGroup = vi.fn(async () => ({ id: 1, createdAt: "t" }));
    const client = new ContactsClient(
      {} as unknown as DiscoveryClient,
      { publishGroup } as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersistWithCommit: async () => {},
      },
    );

    await expect(
      client.setMemberAdmin(null, g.group, ALICE, true),
    ).resolves.toMatchObject({ kind: "proposed" });
    // A sweep empties the pending set (OpenMLS clears it on merge).
    (
      g.group as unknown as { pendingProposals: () => unknown[] }
    ).pendingProposals = (): unknown[] => [];
    await expect(
      client.setMemberAdmin(null, g.group, BOB, true),
    ).resolves.toMatchObject({ kind: "proposed" });
    expect(publishGroup).toHaveBeenCalledTimes(2);
  });
});

describe("why the UI `busy` flag is not the guard (reachability)", () => {
  it("a fire-and-forget refresh clears the shared flag out from under a live commit", async () => {
    // Models routes/Chat.tsx exactly:
    //   refresh()        setBusy(true) @1509, finally setBusy(false) @1704
    //   onAddMember()    setBusy(true) @2208, `void refresh()` @2251, finally @2255
    //   onRemoveMember() setBusy(true) @2272, finally @2333
    let busy = false;
    const poll = deferred<void>();
    const commit = deferred<void>();
    const clickableWhileCommitting: boolean[] = [];

    async function refresh(): Promise<void> {
      busy = true;
      try {
        await poll.promise;
      } finally {
        busy = false;
      }
    }

    async function onAddMember(): Promise<void> {
      busy = true;
      try {
        await Promise.resolve(); // the add commit + publish
        void refresh(); // Chat.tsx:2251 — NOT awaited
      } finally {
        busy = false; // Chat.tsx:2255
      }
    }

    async function onRemoveMember(): Promise<void> {
      busy = true;
      try {
        await commit.promise; // parked mid create-commit -> publish
        clickableWhileCommitting.push(!busy);
      } finally {
        busy = false;
      }
    }

    await onAddMember();
    // The poll is still outstanding, yet the buttons already read enabled.
    expect(busy).toBe(false);

    const removing = onRemoveMember();
    await settle();
    expect(busy).toBe(true);

    // The outstanding refresh lands and clears the SHARED flag while the remove
    // is still in flight: a third commit button is now clickable.
    poll.resolve();
    await settle();
    expect(busy).toBe(false);

    commit.resolve();
    await removing;
    // Observed from inside the still-running commit: the UI reported idle.
    expect(clickableWhileCommitting).toEqual([true]);
  });
});
