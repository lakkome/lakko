// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0040 (G-35) — the restart re-publish sweep of the pending-commit outbox,
 * plus the membership calls that must NOT write to it.
 *
 * ADR-0081 amendment (2026-10-19): the direct-commit paths (add / remove /
 * role change on a group with no role table) whose create -> record -> publish
 * order this suite used to pin are gone. What writes the outbox now is the
 * ADR-0045 sweep, recorded through `onPersistWithCommit` and covered with the
 * sweep; what remains here is (1) a table-less group is refused with the core's
 * typed `noRoleTable` and nothing is sent, (2) a proposal writes no outbox
 * entry, (3) the restart sweep completes an un-acked entry, and (4) a device
 * bound to another account is never proposed. The outbox store and the DS are
 * mocked, so no wasm and no IndexedDB are needed.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeAccessSigner, withCommitRoute, withStagedTwins } from "../ds/access-test-doubles";

// The outbox helpers the client imports (markAcked / loadUnacked) are stubbed so
// the tests need no IndexedDB. The store round-trip itself is covered by
// `src/chat/commit-outbox.test.ts`.
const markAcked = vi.fn(async () => {});
const loadUnacked = vi.fn(async () => [] as unknown[]);
const deleteEntry = vi.fn(async () => {});
const markWelcomesOwed = vi.fn(async () => {});
vi.mock("../chat/commit-outbox", () => ({
  markAcked: (...a: unknown[]) => markAcked(...(a as [])),
  loadUnacked: (...a: unknown[]) => loadUnacked(...(a as [])),
  deleteEntry: (...a: unknown[]) => deleteEntry(...(a as [])),
  markWelcomesOwed: (...a: unknown[]) => markWelcomesOwed(...(a as [])),
}));

// `addMemberToGroup` reads the device's account key via
// `loadWasm().keyPackageAccountKey(...)`; the restart sweep seals a Welcome
// row. Both are stubbed so no wasm core is needed. ADR-0078: a mailbox row is
// sealed to the target's mailbox key; the fake seal marks its bytes so a test
// can see what was sealed, and to what kind.
vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    // ADR-0050: the Welcome is addressed to the added leaf's own key.
    keyPackageSignatureKey: (_kp: Uint8Array) => new Uint8Array(32).fill(9),
    // ADR-0073 R.2: `addMemberToGroup` adds only a device whose binding names
    // the expected account key. The first byte of the fake KeyPackage is the
    // account key's fill, so a test can serve another account's device.
    keyPackageAccountKey: (kp: Uint8Array) => new Uint8Array(32).fill(kp[0] ?? 0),
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

import { ContactsClient, ContactsError, noRoleTableRefusal } from "./client";
import { refusalReasonOf } from "../chat/membership-refusal";
import { base64Encode } from "../auth/account-id";
import type { DsClient } from "../ds/client";
import type { DiscoveryClient } from "../discovery/client";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

/**
 * A stand-in MLS group recording into the shared call-order log. With
 * `roleTable` it carries an ADR-0022 role table and proposes; without one it is
 * the table-less group the ADR-0081 amendment refuses.
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
  return withStagedTwins(base) as unknown as WasmGroup;
}

function makeClient(
  order: string[],
  ds: Partial<DsClient>,
  onPersistWithCommit: (entry: unknown) => Promise<void>,
): ContactsClient {
  return new ContactsClient(
    {} as unknown as DiscoveryClient,
    withCommitRoute({ registerGroupEpoch: async () => {}, ...ds }) as unknown as DsClient,
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

describe("ADR-0045 + ADR-0081 amendment — a membership change is a proposal, never a direct commit", () => {
  it("a group with NO role table is refused with the typed noRoleTable reason; nothing is sent", async () => {
    // ADR-0081 amendment (2026-10-19): the direct-commit path that served a
    // table-less group is gone, and the core refuses such a group's Welcome,
    // so a membership operation on one fails closed with the core's own typed
    // refusal rather than committing anything.
    const order: string[] = [];
    const publishGroup = vi.fn(async () => ({ id: 1, createdAt: "t" }));
    const client = makeClient(order, { publishGroup }, async () => {});

    const err = await client
      .setMemberAdmin(null, mockGroup(order), new Uint8Array(32).fill(2), true)
      .then(
        () => null,
        (e: unknown) => e,
      );
    expect(err).toBeInstanceOf(ContactsError);
    expect(refusalReasonOf(err)).toBe("noRoleTable");
    expect(noRoleTableRefusal("remove")).toBeInstanceOf(ContactsError);
    expect(refusalReasonOf(noRoleTableRefusal("remove"))).toBe("noRoleTable");
    expect(publishGroup).not.toHaveBeenCalled();
    expect(markAcked).not.toHaveBeenCalled();
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
    return withStagedTwins({
      get groupId(): Uint8Array {
        return GROUP_ID;
      },
    }) as unknown as WasmGroup;
  }

  it("an un-acked remove-style entry (commit only) is acked without sending the commit again (ADR-0081)", async () => {
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
      withCommitRoute({ publishGroup, publishMailbox, registerGroupEpoch: async () => {} }) as unknown as DsClient,
      {},
    );
    const rearmed: number[] = [];

    await client.sweepPendingCommits(sweepGroup(), (id) => rearmed.push(id));

    // The entry was written only after the commit route took the commit.
    expect(publishGroup).not.toHaveBeenCalled();
    expect(publishMailbox).not.toHaveBeenCalled();
    expect(rearmed).toEqual([]);
    expect(markAcked).toHaveBeenCalledWith("e1", undefined, undefined);
  });

  it("an add-style entry: only its Welcome is owed, SEALED to the mailbox", async () => {
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
      withCommitRoute({ publishGroup, publishMailbox, registerGroupEpoch: async () => {} }) as unknown as DsClient,
      {},
    );
    const rearmed: number[] = [];

    await client.sweepPendingCommits(sweepGroup(), (id) => rearmed.push(id));

    expect(publishGroup).not.toHaveBeenCalled();
    expect(publishMailbox).toHaveBeenCalledTimes(1);
    // ADR-0078: to the verified target's address, the RAW Welcome sealed as
    // a Welcome row (kind 2) to the target's mailbox key; no clear kind byte.
    expect(publishMailbox.mock.calls[0]?.[0]).toEqual(TARGET.address);
    expect(publishMailbox.mock.calls[0]?.[1]).toEqual(new Uint8Array([0xee, 0x55, 2, 2, 2, 2]));
    expect(rearmed).toEqual([]);
    expect(markAcked).toHaveBeenCalledWith("e2", undefined, 601);
  });

  it("leaves the entry un-acked when the Welcome cannot be delivered (offline DS)", async () => {
    loadUnacked.mockResolvedValueOnce([
      {
        id: "e4",
        accountId: "acct",
        groupId: groupIdB64,
        targetEpoch: "6",
        commitB64: base64Encode(new Uint8Array([1])),
        secondArtifactB64: base64Encode(new Uint8Array([2])),
        secondArtifactKind: "welcome",
        secondArtifactAccountKeyB64: base64Encode(new Uint8Array(32).fill(3)),
        acked: false,
      },
    ] as unknown[]);

    const publishMailbox = vi.fn(async () => {
      throw new Error("DS unreachable");
    });
    const client = new ContactsClient(
      {} as unknown as DiscoveryClient,
      withCommitRoute({ publishGroup: vi.fn(), publishMailbox, registerGroupEpoch: async () => {} }) as unknown as DsClient,
      {},
    );

    await client.sweepPendingCommits(sweepGroup(), () => {});

    expect(publishMailbox).toHaveBeenCalledTimes(1);
    expect(markAcked).not.toHaveBeenCalled();
  });
});

describe("ADR-0073 R.2 — a device bound to another account is never added", () => {
  const GID = new Uint8Array([9, 9, 9, 9]);

  it("addMemberToGroup refuses a device whose binding names another account key: nothing is proposed or published (ADR-0073 R.2)", async () => {
    const epoch = 5n;
    const proposeAddMember = vi.fn(() => new Uint8Array([1]));
    const group = withStagedTwins({
      get groupId(): Uint8Array {
        return GID;
      },
      get epoch(): bigint {
        return epoch;
      },
      dsAccessSigner: fakeAccessSigner(() => epoch),
      roleTableOwner: vi.fn(() => new Uint8Array(32).fill(1)),
      roleTableAdmins: vi.fn(() => new Uint8Array(0)),
      pendingProposals: vi.fn(() => []),
      proposeAddMember,
    }) as unknown as WasmGroup;
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
      withCommitRoute({ publishGroup, publishMailbox, registerGroupEpoch: async () => {} }) as unknown as DsClient,
      { identityProvider: () => IDENTITY_TOKEN },
    );
    await expect(
      client.addMemberToGroup(null, group, new Uint8Array(16).fill(1), new Uint8Array(32).fill(2)),
    ).rejects.toBeInstanceOf(ContactsError);
    expect(proposeAddMember).not.toHaveBeenCalled();
    expect(publishGroup).not.toHaveBeenCalled();
    expect(publishMailbox).not.toHaveBeenCalled();
  });
});
