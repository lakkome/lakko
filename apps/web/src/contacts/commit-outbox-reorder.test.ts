// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0040 (G-35) — the durable pending-commit-outbox reorder on the four
 * `ContactsClient` commit paths, plus the restart re-publish sweep.
 *
 * The bug: a commit path did create-commit (eager merge N -> N+1) -> publish ->
 * best-effort persist. A crash / swallowed persist failure between publish and
 * persist left the commit durable on the DS (peers at N+1) but the local snapshot
 * at N — a silent single-device membership brick.
 *
 * The fix: create-commit -> persist the outbox entry + N+1 snapshot ATOMICALLY,
 * FAIL-CLOSED, BEFORE any publish -> publish -> ack. On restart, sweep un-acked
 * entries and re-publish them idempotently + re-arm the own-echo filter.
 *
 * These tests pin (1) the persist runs BEFORE the publish and a persist FAILURE
 * ABORTS the op (no publish reaches the DS), and (2) the sweep re-publishes an
 * un-acked entry idempotently and re-arms the echo filter for the re-published
 * commit. The outbox store + DS are mocked so no wasm / IndexedDB is needed.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeAccessSigner } from "../ds/access-test-doubles";

// The outbox helpers the client imports (markAcked / loadUnacked) are stubbed so
// the tests need no IndexedDB. The store round-trip itself is covered by
// `src/chat/commit-outbox.test.ts`.
const markAcked = vi.fn(async () => {});
const loadUnacked = vi.fn(async () => [] as unknown[]);
vi.mock("../chat/commit-outbox", () => ({
  markAcked: (...a: unknown[]) => markAcked(...(a as [])),
  loadUnacked: (...a: unknown[]) => loadUnacked(...(a as [])),
}));

// `addMemberToGroup` computes the Welcome's account key via
// `loadWasm().keyPackageAccountKey(...)`. Stub it so the reorder is exercised
// without the real wasm core. The `hoisted` flag lets a test force the
// ARTIFACT-COMPUTE (not the persist) to throw AFTER the eager-merge — the
// ADR-0040 F1 pre-persist residual — so we can assert the reset still fires.
//
// ADR-0045 S7 removed the second such artifact: the external-join approve no
// longer computes a `WelcomeRendezvous` here, because it no longer commits.
// ADR-0078: a mailbox row is sealed to the target's mailbox key; the fake
// seal marks its bytes so a test can see what was sealed, and to what kind.
const hoisted = vi.hoisted(() => ({
  artifactComputeThrows: false,
}));
vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    // ADR-0050: the Welcome is addressed to the added leaf's own key.
    keyPackageSignatureKey: (_kp: Uint8Array) => new Uint8Array(32).fill(9),
    // ADR-0073 R.2: `addMemberToGroup` adds only a device whose binding names
    // the expected account key. The first byte of the fake KeyPackage is the
    // account key's fill, so a test can serve another account's device.
    keyPackageAccountKey: (kp: Uint8Array) => {
      if (hoisted.artifactComputeThrows) {
        throw new Error("keyPackageAccountKey failed (artifact compute)");
      }
      return new Uint8Array(32).fill(kp[0] ?? 0);
    },
    MailboxKind: { ContactRequest: 1, Welcome: 2, ContactAck: 3, WelcomeRendezvous: 4 },
    sealMailboxRow: (pk: Uint8Array, kind: number, body: Uint8Array) =>
      new Uint8Array([0xee, pk[0] ?? 0, kind, ...body]),
  }),
}));

// ADR-0078: the replay resolves the Welcome's target through
// `writeToPerson` (a grant held for the account, its binding verified). The
// grant machinery has its own suites; here it is a fixed verified target.
const TARGET = {
  address: new Uint8Array(32).fill(7),
  mailboxPk: new Uint8Array(32).fill(0x55),
};
vi.mock("../reach/mailbox-write", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../reach/mailbox-write")>()),
  writeToPerson: async (
    _key: Uint8Array,
    _options: unknown,
    write: (target: typeof TARGET, auth: unknown, kind: string) => Promise<unknown>,
  ) => write(TARGET, { signer: null, delegation: null }, "kept"),
}));

import { ContactsClient, ContactsError } from "./client";
import { base64Encode } from "../auth/account-id";
import type { DsClient } from "../ds/client";
import type { DiscoveryClient } from "../discovery/client";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

/**
 * A group whose commit-producing calls return fixed bytes and record into the
 * shared call-order log, standing in for a real MLS group.
 *
 * **`roleTable` defaults to FALSE, and that is load-bearing under ADR-0045.**
 * The ADR-0040 sequence this suite pins — create -> persist -> publish, with a
 * persist failure aborting before any publish — is the DIRECT-commit sequence,
 * and a group carrying an ADR-0022 role table no longer takes it: it routes
 * membership through proposals, which have nothing to persist and nothing to
 * roll back. Leaving the role table on would send these cases down the proposal
 * path, where `removeMember` is never called and the "publish was not reached"
 * assertions would pass for the wrong reason — vacuously green.
 */
function mockGroup(order: string[], opts: { roleTable?: boolean } = {}): WasmGroup {
  const base: Record<string, unknown> = {
    get groupId(): Uint8Array {
      return new Uint8Array([9, 9, 9, 9]);
    },
    get epoch(): bigint {
      return 5n;
    },
    dsAccessSigner: fakeAccessSigner(() => 5n),
    removeMember: vi.fn((_id: unknown, _leaf: number) => {
      order.push("create");
      return new Uint8Array([1, 2, 3]);
    }),
    setGroupRoles: vi.fn((_id: unknown, _owner: Uint8Array, _admins: Uint8Array) => {
      order.push("create");
      return new Uint8Array([4, 5, 6]);
    }),
    pendingProposals: vi.fn(() => []),
  };
  if (opts.roleTable === true) {
    base["roleTableOwner"] = vi.fn(() => new Uint8Array(32).fill(1));
    base["roleTableAdmins"] = vi.fn(() => new Uint8Array(0));
    base["proposeGroupRoles"] = vi.fn(
      (_id: unknown, _owner: Uint8Array, _admins: Uint8Array) => {
        order.push("propose");
        return new Uint8Array([0xaa]);
      },
    );
  } else {
    // A legacy group: no role table at all, so `roleTableOwner` is absent and
    // reading it throws — exactly what a real wasm `Group` without the `0xFF02`
    // extension does, and what `usesProposalPath` treats as "direct commits".
    base["roleTableOwner"] = vi.fn(() => undefined);
    base["roleTableAdmins"] = vi.fn(() => new Uint8Array(0));
  }
  return base as unknown as WasmGroup;
}

function makeClient(
  order: string[],
  ds: Partial<DsClient>,
  onPersistWithCommit: (entry: unknown) => Promise<void>,
): ContactsClient {
  return new ContactsClient(
    {} as unknown as DiscoveryClient,
    { registerGroupEpoch: async () => {}, ...ds } as unknown as DsClient,
    {
      identityProvider: () => IDENTITY_TOKEN,
      onPersistWithCommit: (e) => {
        order.push("persist");
        return onPersistWithCommit(e);
      },
    },
  );
}

afterEach(() => {
  vi.clearAllMocks();
  hoisted.artifactComputeThrows = false;
});

describe("ADR-0040 — commit path persist-before-publish (fail-closed)", () => {
  it("removeMember: persist runs BEFORE publish on the success path", async () => {
    const order: string[] = [];
    const publishGroup = vi.fn(async () => {
      order.push("publish");
      return { id: 42, createdAt: "t" };
    });
    const client = makeClient(order, { publishGroup }, async () => {});

    const res = await client.removeMemberFromGroup(null, mockGroup(order), 1);

    expect(order).toEqual(["create", "persist", "publish"]);
    expect(publishGroup).toHaveBeenCalledTimes(1);
    // ADR-0045 — `mockGroup`'s `roleTableOwner` returns a key, so this group
    // WOULD take the proposal path; the two cases below that must stay on the
    // direct path use a double with no role table. Kept here deliberately: this
    // assertion is what proves the branch is decided by the role table and not
    // by the method called.
    expect(res).toEqual({
      kind: "committed",
      commitMessageId: 42,
      welcomeMessageId: undefined,
    });
    // Acked after the publish.
    expect(markAcked).toHaveBeenCalledWith(expect.any(String), 42, undefined);
  });

  it("removeMember: a persist FAILURE aborts the op — publishGroup is NOT called", async () => {
    const order: string[] = [];
    const publishGroup = vi.fn(async () => {
      order.push("publish");
      return { id: 1, createdAt: "t" };
    });
    const client = makeClient(order, { publishGroup }, async () => {
      throw new Error("IndexedDB write failed");
    });

    await expect(
      client.removeMemberFromGroup(null, mockGroup(order), 1),
    ).rejects.toBeInstanceOf(ContactsError);

    // The whole point: the commit never reached the DS.
    expect(publishGroup).not.toHaveBeenCalled();
    expect(order).toEqual(["create", "persist"]);
    expect(markAcked).not.toHaveBeenCalled();
  });

  it("setMemberAdmin: a group with NO role table is refused outright", async () => {
    // `setMemberAdmin` reads the role table to compute the next admin set, so a
    // group without one has nothing to change — the direct `setGroupRoles` arm
    // that survives in the source is unreachable in practice, and this is what
    // proves it rather than leaving the claim in a comment.
    const order: string[] = [];
    const publishGroup = vi.fn(async () => ({ id: 1, createdAt: "t" }));
    const client = makeClient(order, { publishGroup }, async () => {});

    await expect(
      client.setMemberAdmin(null, mockGroup(order), new Uint8Array(32).fill(2), true),
    ).rejects.toBeInstanceOf(ContactsError);
    expect(publishGroup).not.toHaveBeenCalled();
    expect(order).toEqual([]);
  });

  it("ADR-0045 — setMemberAdmin on a role-table group PROPOSES: no outbox entry, no persist", async () => {
    // A proposal advances no epoch, so there is nothing to roll back and
    // therefore nothing for ADR-0040's outbox to hold. Asserting the ABSENCE of
    // the persist is the point: writing an entry here would put a
    // never-committed artifact into the restart sweep's re-publish set.
    const order: string[] = [];
    const publishGroup = vi.fn(async () => {
      order.push("publish");
      return { id: 9, createdAt: "t" };
    });
    const client = makeClient(order, { publishGroup }, async () => {});

    const res = await client.setMemberAdmin(
      null,
      mockGroup(order, { roleTable: true }),
      new Uint8Array(32).fill(2),
      true,
    );

    expect(res).toEqual({ kind: "proposed", proposalMessageId: 9 });
    expect(order).toEqual(["propose", "publish"]);
    expect(markAcked).not.toHaveBeenCalled();
  });

});

describe("ADR-0040 — restart sweep re-publishes + re-arms the echo filter", () => {
  const GROUP_ID = new Uint8Array([9, 9, 9, 9]);
  const groupIdB64 = base64Encode(GROUP_ID);

  function sweepGroup(): WasmGroup {
    return {
      get groupId(): Uint8Array {
        return GROUP_ID;
      },
    } as unknown as WasmGroup;
  }

  it("re-publishes an un-acked remove-style entry (commit only) + re-arms echo", async () => {
    const commitBytes = new Uint8Array([1, 2, 3]);
    loadUnacked.mockResolvedValueOnce([
      {
        id: "e1",
        accountId: "acct",
        groupId: groupIdB64,
        targetEpoch: "6",
        commitB64: base64Encode(commitBytes),
        acked: false,
      },
    ] as unknown[]);

    const publishGroup = vi.fn(async (_g: Uint8Array, _p: Uint8Array) => ({
      id: 500,
      createdAt: "t",
    }));
    const publishMailbox = vi.fn(async (_g: Uint8Array, _p: Uint8Array) => ({
      id: 0,
      createdAt: "t",
    }));
    const client = new ContactsClient(
      {} as unknown as DiscoveryClient,
      { publishGroup, publishMailbox, registerGroupEpoch: async () => {} } as unknown as DsClient,
      {},
    );
    const rearmed: number[] = [];

    await client.sweepPendingCommits(sweepGroup(), (id) => rearmed.push(id));

    // Re-published the commit to the group journal with the SAME bytes.
    expect(publishGroup).toHaveBeenCalledTimes(1);
    expect(publishGroup.mock.calls[0]?.[0]).toEqual(GROUP_ID);
    expect(publishGroup.mock.calls[0]?.[1]).toEqual(commitBytes);
    // No mailbox artifact for a remove.
    expect(publishMailbox).not.toHaveBeenCalled();
    // Echo re-armed for the re-published commit id.
    expect(rearmed).toEqual([500]);
    // Entry acked.
    expect(markAcked).toHaveBeenCalledWith("e1", 500, undefined);
  });

  it("re-publishes an add-style entry: commit to the journal, Welcome SEALED to the mailbox (no echo re-arm for the mailbox)", async () => {
    const commitBytes = new Uint8Array([1, 1]);
    const welcome = new Uint8Array([2, 2, 2]);
    const accountKey = new Uint8Array(32).fill(3);
    loadUnacked.mockResolvedValueOnce([
      {
        id: "e2",
        accountId: "acct",
        groupId: groupIdB64,
        targetEpoch: "6",
        commitB64: base64Encode(commitBytes),
        secondArtifactB64: base64Encode(welcome),
        secondArtifactKind: "welcome",
        secondArtifactAccountKeyB64: base64Encode(accountKey),
        acked: false,
      },
    ] as unknown[]);

    const publishGroup = vi.fn(async (_g: Uint8Array, _p: Uint8Array) => ({
      id: 600,
      createdAt: "t",
    }));
    const publishMailbox = vi.fn(async (_g: Uint8Array, _p: Uint8Array) => ({
      id: 601,
      createdAt: "t",
    }));
    const client = new ContactsClient(
      {} as unknown as DiscoveryClient,
      { publishGroup, publishMailbox, registerGroupEpoch: async () => {} } as unknown as DsClient,
      {},
    );
    const rearmed: number[] = [];

    await client.sweepPendingCommits(sweepGroup(), (id) => rearmed.push(id));

    // Commit -> group journal, Welcome -> the new member's mailbox.
    expect(publishGroup).toHaveBeenCalledTimes(1);
    expect(publishGroup.mock.calls[0]?.[1]).toEqual(commitBytes);
    expect(publishMailbox).toHaveBeenCalledTimes(1);
    // ADR-0078: to the verified target's address, the RAW Welcome sealed as
    // a Welcome row (kind 2) to the target's mailbox key; no clear kind byte.
    expect(publishMailbox.mock.calls[0]?.[0]).toEqual(TARGET.address);
    expect(publishMailbox.mock.calls[0]?.[1]).toEqual(new Uint8Array([0xee, 0x55, 2, 2, 2, 2]));
    // ONLY the commit id is echo re-armed — the mailbox Welcome never echoes
    // back to the committer's group poll.
    expect(rearmed).toEqual([600]);
    expect(markAcked).toHaveBeenCalledWith("e2", 600, 601);
  });

  it("leaves the entry un-acked when the re-publish fails (offline DS)", async () => {
    loadUnacked.mockResolvedValueOnce([
      {
        id: "e4",
        accountId: "acct",
        groupId: groupIdB64,
        targetEpoch: "6",
        commitB64: base64Encode(new Uint8Array([1])),
        acked: false,
      },
    ] as unknown[]);

    const publishGroup = vi.fn(async () => {
      throw new Error("DS unreachable");
    });
    const client = new ContactsClient(
      {} as unknown as DiscoveryClient,
      { publishGroup, publishMailbox: vi.fn(), registerGroupEpoch: async () => {} } as unknown as DsClient,
      {},
    );

    await client.sweepPendingCommits(sweepGroup(), () => {});

    // Publish attempted, but NOT acked -> the next sweep retries.
    expect(publishGroup).toHaveBeenCalledTimes(1);
    expect(markAcked).not.toHaveBeenCalled();
  });
});

describe("ADR-0040 F1 — reset the in-memory group on a commit persist-abort", () => {
  const GID = new Uint8Array([9, 9, 9, 9]);
  const GID_B64 = base64Encode(GID);

  /** A stateful stand-in for an MLS group whose remove-commit eager-merges the
   *  epoch N -> N+1 (exactly as the real core does inside `removeMember`),
   *  exposing the live epoch so the tests can observe whether the F1 reset rolled
   *  it back. `setEpoch` models the real reload-from-durable-snapshot (which
   *  `chats.resetGroupToDurableSnapshot` performs by replacing the cached group
   *  with one `Group.load`ed at epoch N). */
  function statefulGroup(startEpoch: bigint): {
    group: WasmGroup;
    epoch: () => bigint;
    setEpoch: (e: bigint) => void;
  } {
    let epoch = startEpoch;
    const group = {
      get groupId(): Uint8Array {
        return GID;
      },
      get epoch(): bigint {
        return epoch;
      },
      dsAccessSigner: fakeAccessSigner(() => epoch),
      removeMember: vi.fn((_id: unknown, _leaf: number) => {
        epoch += 1n; // eager merge N -> N+1
        return new Uint8Array([1, 2, 3]);
      }),
    } as unknown as WasmGroup;
    return {
      group,
      epoch: () => epoch,
      setEpoch: (e: bigint) => {
        epoch = e;
      },
    };
  }

  it("a transient persist failure rolls the in-memory epoch back to N; the retry builds on N, not the poisoned N+1", async () => {
    const N = 5n;
    const g = statefulGroup(N);
    const publishGroup = vi.fn(async () => ({ id: 1, createdAt: "t" }));

    const persistTargetEpochs: string[] = [];
    let persistCalls = 0;
    const abortHookGids: string[] = [];

    const client = new ContactsClient(
      {} as unknown as DiscoveryClient,
      { publishGroup, registerGroupEpoch: async () => {} } as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersistWithCommit: async (e) => {
          persistCalls += 1;
          persistTargetEpochs.push((e as { targetEpoch: string }).targetEpoch);
          // The FIRST persist fails (a transient IndexedDB quota hiccup that
          // then clears — the exact F1 scenario, NOT a crash).
          if (persistCalls === 1) {
            throw new Error("IndexedDB quota hiccup (transient)");
          }
        },
        // Model chats.resetGroupToDurableSnapshot: reload the group at the last
        // durable epoch (N). The observable effect is the epoch roll-back.
        onCommitPersistAbort: async (gid) => {
          abortHookGids.push(gid);
          g.setEpoch(N);
        },
      },
    );

    // Op A: create-commit eager-merges 5 -> 6, then the persist throws -> abort.
    await expect(
      client.removeMemberFromGroup(null, g.group, 1),
    ).rejects.toBeInstanceOf(ContactsError);

    // The reset hook fired for THIS group id, and the in-memory epoch is back at
    // N (5) — NOT the poisoned, un-published N+1 (6).
    expect(abortHookGids).toEqual([GID_B64]);
    expect(g.epoch()).toBe(N);
    // The commit never reached the DS (fail-closed abort).
    expect(publishGroup).not.toHaveBeenCalled();
    // The aborted op's entry recorded the eager-merged N+1 (6).
    expect(persistTargetEpochs[0]).toBe("6");

    // Op B — the user retries in the SAME session. It must build on the reset N
    // (5) and eager-merge 5 -> 6, NOT 6 -> 7. A 6 -> 7 commit would publish while
    // peers are still at N and strand the whole group (the F1 bug).
    const res = await client.removeMemberFromGroup(null, g.group, 1);
    expect(g.epoch()).toBe(6n);
    expect(persistTargetEpochs[1]).toBe("6"); // N+1 of the reset N, not "7"
    expect(publishGroup).toHaveBeenCalledTimes(1);
    expect(res.kind === "committed" ? res.commitMessageId : -1).toBe(1);
  });

  it("the happy path leaves the group at N+1 and never fires the reset hook", async () => {
    const g = statefulGroup(5n);
    const publishGroup = vi.fn(async () => ({ id: 7, createdAt: "t" }));
    const abortHook = vi.fn(async () => {});
    const client = new ContactsClient(
      {} as unknown as DiscoveryClient,
      { publishGroup, registerGroupEpoch: async () => {} } as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersistWithCommit: async () => {},
        onCommitPersistAbort: abortHook,
      },
    );

    await client.removeMemberFromGroup(null, g.group, 1);

    // Published + persisted -> the group correctly stays at N+1; no reset.
    expect(g.epoch()).toBe(6n);
    expect(abortHook).not.toHaveBeenCalled();
    expect(publishGroup).toHaveBeenCalledTimes(1);
  });

  it("FAIL-SAFE: when the reset itself fails, the op rejects with a hard error (never swallowed) and does not publish", async () => {
    const g = statefulGroup(5n);
    const publishGroup = vi.fn(async () => ({ id: 1, createdAt: "t" }));
    const client = new ContactsClient(
      {} as unknown as DiscoveryClient,
      { publishGroup, registerGroupEpoch: async () => {} } as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersistWithCommit: async () => {
          throw new Error("IndexedDB write failed");
        },
        onCommitPersistAbort: async () => {
          // e.g. no durable snapshot for the group — the reset cannot recover it.
          throw new Error("no durable snapshot");
        },
      },
    );

    // The reset-failure error (not the original persist error) surfaces — a hard,
    // distinct signal that the group may still be poisoned. Never swallowed.
    await expect(
      client.removeMemberFromGroup(null, g.group, 1),
    ).rejects.toThrowError(/reset FAILED/);
    expect(publishGroup).not.toHaveBeenCalled();
  });

  it("with no reset hook wired (standalone / test callers), a persist failure still aborts cleanly", async () => {
    const g = statefulGroup(5n);
    const publishGroup = vi.fn(async () => ({ id: 1, createdAt: "t" }));
    const client = new ContactsClient(
      {} as unknown as DiscoveryClient,
      { publishGroup, registerGroupEpoch: async () => {} } as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersistWithCommit: async () => {
          throw new Error("disk full");
        },
        // onCommitPersistAbort intentionally UNSET.
      },
    );

    await expect(
      client.removeMemberFromGroup(null, g.group, 1),
    ).rejects.toBeInstanceOf(ContactsError);
    expect(publishGroup).not.toHaveBeenCalled();
    // No cache to reset -> the in-memory epoch stays at the un-published N+1.
    // Such callers persist nothing + hold no cached group, so they are outside
    // the desync blast radius (the documented no-op path).
    expect(g.epoch()).toBe(6n);
  });

  // The F1 pre-persist RESIDUAL: in the two-artifact paths, code between the
  // eager-merge and the persist can ALSO throw (the Welcome-envelope / rendezvous
  // computation). Before the widened reset region it aborted WITHOUT a reset,
  // leaving the group ahead at the un-published N+1. These two tests force the
  // ARTIFACT-COMPUTE (not the persist) to throw and assert the reset still fires.

  it("addMemberToGroup: the Welcome-artifact compute throws AFTER the merge -> epoch rolls back to N; neither artifact reaches the DS", async () => {
    let armArtifactThrow = false;
    const N = 5n;
    let epoch = N;
    const group = {
      get groupId(): Uint8Array {
        return GID;
      },
      get epoch(): bigint {
        return epoch;
      },
      dsAccessSigner: fakeAccessSigner(() => epoch),
      addMember: vi.fn((_id: unknown, _kp: Uint8Array) => {
        epoch += 1n; // eager merge N -> N+1
        // The artifact compute after the merge throws.
        hoisted.artifactComputeThrows = armArtifactThrow;
        return {
          commit: new Uint8Array([1, 2, 3]),
          welcome: new Uint8Array([4, 4]),
        };
      }),
    } as unknown as WasmGroup;

    const publishGroup = vi.fn(async () => ({ id: 1, createdAt: "t" }));
    const publishMailbox = vi.fn(async () => ({ id: 2, createdAt: "t" }));
    const abortHookGids: string[] = [];
    let persistCalled = false;
    const client = new ContactsClient(
      // addMemberToGroup checks the new member's published identity (ADR-0073
      // phase 2; nothing published passes) and fetches their KeyPackage first.
      {
        identityFetch: async () => null,
        fetchKeyPackages: async () => [
          { deviceId: new Uint8Array(32).fill(9), keyPackage: new Uint8Array([2, 9, 9]) },
        ],
      } as unknown as DiscoveryClient,
      { publishGroup, publishMailbox, registerGroupEpoch: async () => {} } as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersistWithCommit: async () => {
          persistCalled = true;
        },
        onCommitPersistAbort: async (gid) => {
          abortHookGids.push(gid);
          epoch = N;
        },
      },
    );

    // The merge advances 5 -> 6, then `keyPackageAccountKey(...)` throws BEFORE
    // the persist runs — the residual. The op must STILL reset to N.
    armArtifactThrow = true;
    await expect(
      client.addMemberToGroup(
        null,
        group,
        new Uint8Array(16).fill(1),
        new Uint8Array(32).fill(2),
      ),
    ).rejects.toBeInstanceOf(ContactsError);

    // Reset fired off the poisoned N+1; the persist never ran (throw was before
    // it); NEITHER artifact reached the DS.
    expect(persistCalled).toBe(false);
    expect(abortHookGids).toEqual([GID_B64]);
    expect(epoch).toBe(N);
    expect(publishGroup).not.toHaveBeenCalled();
    expect(publishMailbox).not.toHaveBeenCalled();
  });
  it("addMemberToGroup refuses a device whose binding names another account key: nothing is merged or published (ADR-0073 R.2)", async () => {
    let epoch = 5n;
    const addMember = vi.fn(() => {
      epoch += 1n;
      return { commit: new Uint8Array([1]), welcome: new Uint8Array([2]) };
    });
    const group = {
      get groupId(): Uint8Array {
        return GID;
      },
      get epoch(): bigint {
        return epoch;
      },
      dsAccessSigner: fakeAccessSigner(() => epoch),
      addMember,
    } as unknown as WasmGroup;
    const publishGroup = vi.fn(async () => ({ id: 1, createdAt: "t" }));
    const publishMailbox = vi.fn(async () => ({ id: 2, createdAt: "t" }));
    const client = new ContactsClient(
      {
        identityFetch: async () => null,
        // The directory serves, under the asked account id, a device bound to
        // account key 0x07..., not to the expected 0x02...
        fetchKeyPackages: async () => [
          { deviceId: new Uint8Array(32).fill(9), keyPackage: new Uint8Array([7, 9, 9]) },
        ],
      } as unknown as DiscoveryClient,
      { publishGroup, publishMailbox, registerGroupEpoch: async () => {} } as unknown as DsClient,
      { identityProvider: () => IDENTITY_TOKEN },
    );
    await expect(
      client.addMemberToGroup(null, group, new Uint8Array(16).fill(1), new Uint8Array(32).fill(2)),
    ).rejects.toBeInstanceOf(ContactsError);
    expect(addMember).not.toHaveBeenCalled();
    expect(epoch).toBe(5n);
    expect(publishGroup).not.toHaveBeenCalled();
    expect(publishMailbox).not.toHaveBeenCalled();
  });
});
