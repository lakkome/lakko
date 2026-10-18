// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0053 contract point 3 on the DIRECT commit paths, as ADR-0081 builds
 * them: the commit is STAGED (the group stays at N), the ADR-0040 entry is
 * written with the snapshot that carries it, and then the key of N+1 (from
 * the staged commit, `dsStagedAccessSigner`) and the commit row reach the DS
 * in one request. A refused registration abandons the staged commit: the
 * group stays at N, nothing else is sent, no reload is needed.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

const markAcked = vi.fn(async () => {});
const loadUnacked = vi.fn(async () => [] as unknown[]);
const deleteEntry = vi.fn(async () => {});
vi.mock("../chat/commit-outbox", () => ({
  markAcked: (...a: unknown[]) => markAcked(...(a as [])),
  loadUnacked: (...a: unknown[]) => loadUnacked(...(a as [])),
  deleteEntry: (...a: unknown[]) => deleteEntry(...(a as [])),
}));

import { ContactsClient } from "./client";
import type { DsClient } from "../ds/client";
import type { DiscoveryClient } from "../discovery/client";
import { fakeAccessSigner, withCommitRoute, withStagedTwins } from "../ds/access-test-doubles";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

/** A table-less group (so membership takes the direct-commit arm) whose
 *  create calls eager-merge N -> N+1 like the core. */
function directGroup(order: string[]): { group: WasmGroup; epoch: () => bigint } {
  let epoch = 5n;
  const group = withStagedTwins({
    get groupId(): Uint8Array {
      return new Uint8Array([4, 4, 4, 4]);
    },
    get epoch(): bigint {
      return epoch;
    },
    dsAccessSigner: fakeAccessSigner(() => epoch),
    roleTableOwner: () => undefined,
    roleTableAdmins: () => new Uint8Array(0),
    pendingProposals: () => [],
    removeMember: vi.fn(() => {
      epoch += 1n;
      order.push("create");
      return new Uint8Array([1, 2, 3]);
    }),
  }) as unknown as WasmGroup;
  return { group, epoch: () => epoch };
}

function client(
  order: string[],
  registerFails: boolean,
  resets: string[],
): ContactsClient {
  const ds = withCommitRoute({
    registerGroupEpoch: vi.fn(async (_g: Uint8Array, epoch: bigint) => {
      order.push(`register:${epoch}`);
      if (registerFails) throw Object.assign(new Error("ds access: 401 proof required"), { status: 401 });
    }),
    publishGroup: vi.fn(async () => {
      order.push("publish");
      return { id: 11, createdAt: "t" };
    }),
  }) as unknown as DsClient;
  return new ContactsClient({} as unknown as DiscoveryClient, ds, {
    identityProvider: () => IDENTITY_TOKEN,
    onPersistWithCommit: async () => {
      order.push("persist");
    },
    onCommitPersistAbort: async (gid) => {
      resets.push(gid);
    },
  });
}

afterEach(() => {
  vi.clearAllMocks();
});

describe("ADR-0053 contract point 3 + ADR-0081 — a direct commit registers N+1 with its row", () => {
  it("remove: stage -> outbox -> (register N+1 + publish, one request) -> merge", async () => {
    const order: string[] = [];
    const g = directGroup(order);
    const out = await client(order, false, []).removeMemberFromGroup(null, g.group, 1);
    expect(out.kind).toBe("committed");
    expect(order).toEqual(["create", "persist", "register:6", "publish"]);
    expect(g.group.epoch).toBe(6n);
  });

  it("a refused registration abandons the staged commit: no publish, the group stays at N", async () => {
    const order: string[] = [];
    const resets: string[] = [];
    const g = directGroup(order);
    await expect(
      client(order, true, resets).removeMemberFromGroup(null, g.group, 1),
    ).rejects.toThrow(/proof required/);
    expect(order).toEqual(["create", "persist", "register:6"]);
    expect(order).not.toContain("publish");
    expect(g.group.epoch).toBe(5n);
    expect(g.group.hasStagedSweep()).toBe(false);
    expect(deleteEntry).toHaveBeenCalledTimes(1);
    expect(resets).toHaveLength(0);
  });
});
