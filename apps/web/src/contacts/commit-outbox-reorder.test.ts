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

// The outbox helpers the client imports (markAcked / loadUnacked) are stubbed so
// the tests need no IndexedDB. The store round-trip itself is covered by
// `src/chat/commit-outbox.test.ts`.
const markAcked = vi.fn(async () => {});
const loadUnacked = vi.fn(async () => [] as unknown[]);
vi.mock("../chat/commit-outbox", () => ({
  markAcked: (...a: unknown[]) => markAcked(...(a as [])),
  loadUnacked: (...a: unknown[]) => loadUnacked(...(a as [])),
}));

// `approveExternalJoin` computes the rendezvous via `loadWasm().wrapWelcomeRendezvous`.
// Stub it so the reorder is exercised without the real wasm core.
vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    wrapWelcomeRendezvous: (ref: Uint8Array, welcome: Uint8Array): Uint8Array =>
      new Uint8Array([0xff, ...ref.slice(0, 2), ...welcome]),
    IdentityPseudonym: {
      fromIdentity: (_pk: Uint8Array) => ({
        asBytes: (): Uint8Array => new Uint8Array(32).fill(7),
      }),
    },
  }),
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

/** A group whose commit-producing calls return fixed bytes and record into the
 *  shared call-order log, standing in for a real MLS group. */
function mockGroup(order: string[]): WasmGroup {
  return {
    get groupId(): Uint8Array {
      return new Uint8Array([9, 9, 9, 9]);
    },
    get epoch(): bigint {
      return 5n;
    },
    removeMember: vi.fn((_id: unknown, _leaf: number) => {
      order.push("create");
      return new Uint8Array([1, 2, 3]);
    }),
    setGroupRoles: vi.fn((_id: unknown, _owner: Uint8Array, _admins: Uint8Array) => {
      order.push("create");
      return new Uint8Array([4, 5, 6]);
    }),
    roleTableOwner: vi.fn(() => new Uint8Array(32).fill(1)),
    roleTableAdmins: vi.fn(() => new Uint8Array(0)),
    approveExternalJoin: vi.fn((_id: unknown, _ref: Uint8Array) => {
      order.push("create");
      return { commit: new Uint8Array([7, 7]), welcome: new Uint8Array([8, 8]) };
    }),
  } as unknown as WasmGroup;
}

function makeClient(
  order: string[],
  ds: Partial<DsClient>,
  onPersistWithCommit: (entry: unknown) => Promise<void>,
): ContactsClient {
  return new ContactsClient(
    {} as unknown as DiscoveryClient,
    ds as unknown as DsClient,
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
    expect(res.commitMessageId).toBe(42);
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

  it("setMemberAdmin: a persist FAILURE aborts the op — publishGroup is NOT called", async () => {
    const order: string[] = [];
    const publishGroup = vi.fn(async () => ({ id: 1, createdAt: "t" }));
    const client = makeClient(order, { publishGroup }, async () => {
      throw new Error("disk full");
    });

    await expect(
      client.setMemberAdmin(null, mockGroup(order), new Uint8Array(32).fill(2), true),
    ).rejects.toBeInstanceOf(ContactsError);
    expect(publishGroup).not.toHaveBeenCalled();
  });

  it("approveExternalJoin (two-artifact): a persist FAILURE aborts BEFORE either publish", async () => {
    const order: string[] = [];
    const publishGroup = vi.fn(async () => {
      order.push("publish");
      return { id: 1, createdAt: "t" };
    });
    const client = makeClient(order, { publishGroup }, async () => {
      throw new Error("quota exceeded");
    });

    await expect(
      client.approveExternalJoin(null, mockGroup(order), new Uint8Array(32).fill(3)),
    ).rejects.toBeInstanceOf(ContactsError);

    // Neither the commit nor the rendezvous reached the DS — the entry (holding
    // BOTH) failed to persist, so nothing publishes.
    expect(publishGroup).not.toHaveBeenCalled();
    expect(order).toEqual(["create", "persist"]);
  });

  it("approveExternalJoin: publishes BOTH artifacts and acks with both ids on success", async () => {
    const order: string[] = [];
    let n = 100;
    const publishGroup = vi.fn(async () => {
      order.push("publish");
      return { id: (n += 1), createdAt: "t" };
    });
    const client = makeClient(order, { publishGroup }, async () => {});

    const res = await client.approveExternalJoin(
      null,
      mockGroup(order),
      new Uint8Array(32).fill(3),
    );

    // create -> persist -> publish(commit) -> publish(rendezvous).
    expect(order).toEqual(["create", "persist", "publish", "publish"]);
    expect(publishGroup).toHaveBeenCalledTimes(2);
    expect(res.commitMessageId).toBe(101);
    expect(res.welcomeMessageId).toBe(102);
    expect(markAcked).toHaveBeenCalledWith(expect.any(String), 101, 102);
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
      { publishGroup, publishMailbox } as unknown as DsClient,
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

  it("re-publishes an add-style entry: commit to the journal, Welcome to the mailbox (no echo re-arm for the mailbox)", async () => {
    const commitBytes = new Uint8Array([1, 1]);
    const welcomeEnvelope = new Uint8Array([2, 2, 2]);
    const mailbox = new Uint8Array(32).fill(7);
    loadUnacked.mockResolvedValueOnce([
      {
        id: "e2",
        accountId: "acct",
        groupId: groupIdB64,
        targetEpoch: "6",
        commitB64: base64Encode(commitBytes),
        secondArtifactB64: base64Encode(welcomeEnvelope),
        secondArtifactKind: "welcome",
        secondArtifactMailboxB64: base64Encode(mailbox),
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
      { publishGroup, publishMailbox } as unknown as DsClient,
      {},
    );
    const rearmed: number[] = [];

    await client.sweepPendingCommits(sweepGroup(), (id) => rearmed.push(id));

    // Commit -> group journal, Welcome -> the new member's mailbox.
    expect(publishGroup).toHaveBeenCalledTimes(1);
    expect(publishGroup.mock.calls[0]?.[1]).toEqual(commitBytes);
    expect(publishMailbox).toHaveBeenCalledTimes(1);
    expect(publishMailbox.mock.calls[0]?.[0]).toEqual(mailbox);
    expect(publishMailbox.mock.calls[0]?.[1]).toEqual(welcomeEnvelope);
    // ONLY the commit id is echo re-armed — the mailbox Welcome never echoes
    // back to the committer's group poll.
    expect(rearmed).toEqual([600]);
    expect(markAcked).toHaveBeenCalledWith("e2", 600, 601);
  });

  it("re-publishes an approve-style entry: commit + rendezvous both to the journal", async () => {
    const commitBytes = new Uint8Array([3, 3]);
    const rendezvous = new Uint8Array([4, 4, 4, 4]);
    loadUnacked.mockResolvedValueOnce([
      {
        id: "e3",
        accountId: "acct",
        groupId: groupIdB64,
        targetEpoch: "6",
        commitB64: base64Encode(commitBytes),
        secondArtifactB64: base64Encode(rendezvous),
        secondArtifactKind: "rendezvous",
        acked: false,
      },
    ] as unknown[]);

    let n = 700;
    const publishGroup = vi.fn(async () => ({ id: (n += 1), createdAt: "t" }));
    const publishMailbox = vi.fn(async () => ({ id: 0, createdAt: "t" }));
    const client = new ContactsClient(
      {} as unknown as DiscoveryClient,
      { publishGroup, publishMailbox } as unknown as DsClient,
      {},
    );
    const rearmed: number[] = [];

    await client.sweepPendingCommits(sweepGroup(), (id) => rearmed.push(id));

    expect(publishGroup).toHaveBeenCalledTimes(2);
    expect(publishMailbox).not.toHaveBeenCalled();
    // Only the commit id is echo re-armed (the rendezvous benign-drops in the
    // receive loop's catch either way).
    expect(rearmed).toEqual([701]);
    expect(markAcked).toHaveBeenCalledWith("e3", 701, 702);
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
      { publishGroup, publishMailbox: vi.fn() } as unknown as DsClient,
      {},
    );

    await client.sweepPendingCommits(sweepGroup(), () => {});

    // Publish attempted, but NOT acked -> the next sweep retries.
    expect(publishGroup).toHaveBeenCalledTimes(1);
    expect(markAcked).not.toHaveBeenCalled();
  });
});
