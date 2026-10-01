// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0053 contract point 3 on the DIRECT (eager-merge) commit paths: the key
 * of the epoch a commit moves to is registered (since ADR-0057 with this
 * device's root or registrar key, which the DS client resolves) BEFORE the
 * ADR-0040 outbox write and the publish — so a
 * commit is never on the wire before the server knows its epoch, and every
 * peer that applies it can still reach the journal. A refused registration
 * aborts exactly as a failed outbox write does: the group is reset to N and
 * nothing is published.
 *
 * The group double's `dsAccessSigner` reads the LIVE epoch, so the test sees
 * that the key registered is the one taken after the create (N+1).
 */
import { afterEach, describe, expect, it, vi } from "vitest";

const markAcked = vi.fn(async () => {});
const loadUnacked = vi.fn(async () => [] as unknown[]);
vi.mock("../chat/commit-outbox", () => ({
  markAcked: (...a: unknown[]) => markAcked(...(a as [])),
  loadUnacked: (...a: unknown[]) => loadUnacked(...(a as [])),
}));

import { ContactsClient } from "./client";
import type { DsClient } from "../ds/client";
import type { DiscoveryClient } from "../discovery/client";
import { fakeAccessSigner } from "../ds/access-test-doubles";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

/** A table-less group (so membership takes the direct-commit arm) whose
 *  create calls eager-merge N -> N+1 like the core. */
function directGroup(order: string[]): { group: WasmGroup; epoch: () => bigint } {
  let epoch = 5n;
  const group = {
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
  } as unknown as WasmGroup;
  return { group, epoch: () => epoch };
}

function client(
  order: string[],
  registerFails: boolean,
  resets: string[],
): ContactsClient {
  const ds = {
    registerGroupEpoch: vi.fn(async (_g: Uint8Array, epoch: bigint) => {
      order.push(`register:${epoch}`);
      if (registerFails) throw new Error("ds access: 401 proof required");
    }),
    publishGroup: vi.fn(async () => {
      order.push("publish");
      return { id: 11, createdAt: "t" };
    }),
  } as unknown as DsClient;
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

describe("ADR-0053 contract point 3 — direct commits register before they publish", () => {
  it("remove: create -> register N+1 -> outbox -> publish", async () => {
    const order: string[] = [];
    const g = directGroup(order);
    const out = await client(order, false, []).removeMemberFromGroup(null, g.group, 1);
    expect(out.kind).toBe("committed");
    expect(order).toEqual(["create", "register:6", "persist", "publish"]);
  });

  it("a refused registration aborts: no outbox write, no publish, the group is reset to N", async () => {
    const order: string[] = [];
    const resets: string[] = [];
    const g = directGroup(order);
    await expect(
      client(order, true, resets).removeMemberFromGroup(null, g.group, 1),
    ).rejects.toThrow(/proof required/);
    expect(order).toEqual(["create", "register:6"]);
    expect(order).not.toContain("persist");
    expect(order).not.toContain("publish");
    expect(resets).toHaveLength(1);
  });
});
