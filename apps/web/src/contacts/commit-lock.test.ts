// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 — membership changes under the per-group MLS lock, and why the UI
 * `busy` flag is not the guard.
 *
 * ADR-0081 amendment (2026-10-19): the four direct create-commit -> publish
 * regions this suite used to serialize are gone (a group with no role table
 * is refused; every membership change is a proposal, and only the sweep
 * commits). The lock itself lives in `src/mls/group-lock.ts` and is covered
 * by `chat/poll-lock.test.ts` and `channels/copublisher-lock.test.ts`; what
 * remains here is the role-change refusal while one is pending, and the
 * reachability of the `busy` flag hazard.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeAccessSigner, withCommitRoute, withStagedTwins } from "../ds/access-test-doubles";

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

afterEach(() => {
  vi.clearAllMocks();
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
    const group = withStagedTwins({
      get groupId(): Uint8Array {
        return GID;
      },
      get epoch(): bigint {
        return epoch;
      },
      dsAccessSigner: fakeAccessSigner(() => epoch),
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
    }) as unknown as WasmGroup;
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
      withCommitRoute({ publishGroup, registerGroupEpoch: async () => {} }) as unknown as DsClient,
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
      withCommitRoute({ publishGroup, registerGroupEpoch: async () => {} }) as unknown as DsClient,
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
