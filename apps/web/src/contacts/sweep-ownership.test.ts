// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 obligation 5 on the ADR-0045 sweep adapter (`contacts/client.ts`):
 * the staged epoch is registered through the DS client (which signs with this
 * device's root or registrar key), and the OWNER's registration revokes the
 * registrar keys of every account the sweep evicts — derived from the root
 * seed with the REAL core — while an admin, who cannot derive them, revokes
 * the keys it collected from the owner's registrar-seed envelopes, lower
 * ranks only (ADR-0057, amended 2026-10-01, item 1). A committer with
 * neither root nor seed does not stage at all.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

vi.mock("../chat/commit-outbox", () => ({
  markAcked: vi.fn(async () => {}),
  loadUnacked: vi.fn(async () => [] as unknown[]),
  deleteEntry: vi.fn(async () => {}),
}));

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { ContactsClient } from "./client";
import { dsAccess } from "../ds/access";
import { bindRegistrarListAccount, recordRegistrarListEntry } from "../ds/registrar-list";
import { fakeAccessSigner, fakeRegistrarAuthority, fakeStatementSigner, withCommitRoute } from "../ds/access-test-doubles";
import type { DsClient } from "../ds/client";
import type { DiscoveryClient } from "../discovery/client";
import type { ActingCommitter } from "../chat/committer-beacon";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;
const acct = (n: number): Uint8Array => new Uint8Array(32).fill(n);
const CURSOR = { read: (): number => 0, advance: (): void => {} };
const ACT = async (): Promise<ActingCommitter> => ({ kind: "act" });

let seq = 0;
function sweepingGroup(): { group: WasmGroup; order: string[] } {
  seq += 1;
  const gid = new Uint8Array(32).fill(0x60 + seq);
  const order: string[] = [];
  let epoch = 6n;
  let staged = false;
  let pending = 1;
  const group = {
    get groupId(): Uint8Array {
      return gid;
    },
    get epoch(): bigint {
      return epoch;
    },
    dsAccessSigner: fakeAccessSigner(() => epoch),
    dsStagedAccessSigner: () => (staged ? fakeAccessSigner(() => epoch + 1n)(undefined) : undefined),
    roleTableOwner: () => acct(1),
    roleTableAdmins: () => new Uint8Array(0),
    ownCommitterRank: () => 0,
    members: () => [
      { leafIndex: 0, signaturePublicKey: acct(0xa0), accountKey: acct(1) },
      { leafIndex: 1, signaturePublicKey: acct(0xa1), accountKey: acct(2) },
      // Account 3 has two devices; the sweep removes both.
      { leafIndex: 2, signaturePublicKey: acct(0xa2), accountKey: acct(3) },
      { leafIndex: 3, signaturePublicKey: acct(0xa3), accountKey: acct(3) },
    ],
    pendingProposals: () => Array.from({ length: pending }, () => ({ kind: "remove" })),
    hasStagedSweep: () => staged,
    stagePendingProposals: () => {
      order.push("stage");
      staged = true;
      return {
        commit: new Uint8Array([0x5e]),
        welcome: undefined,
        swept: () => [
          { kind: "remove", removeTargetLeaf: 2 },
          { kind: "leave", senderLeaf: 3 },
        ],
      };
    },
    mergeStagedSweep: () => {
      order.push("merge");
      staged = false;
      epoch += 1n;
      pending = 0;
    },
    abandonStagedSweep: () => {
      order.push("abandon");
      staged = false;
    },
    processIncomingVerifyingRoles: () => ({ kind: "Commit" }),
  } as unknown as WasmGroup;
  return { group, order };
}

function client(registrations: Array<{ epoch: bigint; revoke: Uint8Array[] }>): ContactsClient {
  let id = 100;
  const ds = withCommitRoute({
    registerGroupEpoch: vi.fn(async (_g: Uint8Array, epoch: bigint, _k: Uint8Array, revoke: Uint8Array[] = []) => {
      registrations.push({ epoch, revoke: [...revoke] });
    }),
    publishGroup: vi.fn(async () => ({ id: (id += 1), createdAt: "t" })),
    publishMailbox: vi.fn(async () => ({ id: (id += 1), createdAt: "t" })),
    fetchGroup: vi.fn(async () => ({ messages: [], hasMore: false })),
  }) as unknown as DsClient;
  return new ContactsClient({} as unknown as DiscoveryClient, ds, {
    identityProvider: () => IDENTITY_TOKEN,
    onPersist: async () => {},
    onPersistWithCommit: async () => {},
  });
}

/** The `localStorage` shim the registrar list reads. */
class MemoryStorage {
  private readonly map = new Map<string, string>();
  get length(): number {
    return this.map.size;
  }
  key(i: number): string | null {
    return [...this.map.keys()][i] ?? null;
  }
  getItem(k: string): string | null {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, v);
  }
  removeItem(k: string): void {
    this.map.delete(k);
  }
}

let offOwner: (() => void) | null = null;
afterEach(() => {
  offOwner?.();
  offOwner = null;
  dsAccess.setAuthorityFactory(null);
  bindRegistrarListAccount(null);
  delete (globalThis as unknown as { localStorage?: unknown }).localStorage;
});

const b64 = (b: Uint8Array): string => Buffer.from(b).toString("base64");

describe("ADR-0057 — the sweep's registration and its revocations", () => {
  it("the owner revokes the registrar key of each account the sweep evicts, and only those", async () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x21));
    const g = sweepingGroup();
    const rootSeed = wasm.dsJournalRootSeed(owner.ed25519Seed, g.group.groupId);
    offOwner = dsAccess.registerOwnerSource(() => true);
    dsAccess.setAuthorityFactory({
      root: () => ({ kind: "root", signer: fakeStatementSigner("root"), rootSeed }),
      registrar: () => ({ kind: "none" }),
    });
    const registrations: Array<{ epoch: bigint; revoke: Uint8Array[] }> = [];

    const out = await client(registrations).sweepIfCommitter(null, g.group, CURSOR, () => {}, ACT, () => {});

    expect(out.kind).toBe("committed");
    expect(registrations).toHaveLength(1);
    expect(registrations[0]?.epoch).toBe(7n);
    const expected = wasm.dsStatementSigner(wasm.dsJournalRegistrarSeed(rootSeed, acct(3)));
    expect(registrations[0]?.revoke).toEqual([expected.publicKey()]);
    expected.free();
    owner.free();
  });

  it("an admin revokes the collected registrar key of an evicted MEMBER-rank account", async () => {
    (globalThis as unknown as { localStorage: unknown }).localStorage = new MemoryStorage();
    const g = sweepingGroup();
    bindRegistrarListAccount("acc");
    const memberKey = new Uint8Array(32).fill(0x33);
    recordRegistrarListEntry("acc", b64(g.group.groupId), b64(acct(3)), { publicKey: memberKey, rank: 1 });
    // A collected key for an account the sweep does NOT evict is never sent.
    recordRegistrarListEntry("acc", b64(g.group.groupId), b64(acct(2)), {
      publicKey: new Uint8Array(32).fill(0x22),
      rank: 1,
    });
    dsAccess.setAuthorityFactory({
      root: () => ({ kind: "none" }),
      registrar: () => fakeRegistrarAuthority("admin", 2),
    });
    const registrations: Array<{ epoch: bigint; revoke: Uint8Array[] }> = [];
    const out = await client(registrations).sweepIfCommitter(null, g.group, CURSOR, () => {}, ACT, () => {});
    expect(out.kind).toBe("committed");
    expect(registrations).toEqual([{ epoch: 7n, revoke: [memberKey] }]);
  });

  it("an admin skips an evicted account of EQUAL rank (the DS would refuse it)", async () => {
    (globalThis as unknown as { localStorage: unknown }).localStorage = new MemoryStorage();
    const g = sweepingGroup();
    bindRegistrarListAccount("acc");
    recordRegistrarListEntry("acc", b64(g.group.groupId), b64(acct(3)), {
      publicKey: new Uint8Array(32).fill(0x33),
      rank: 2,
    });
    dsAccess.setAuthorityFactory({
      root: () => ({ kind: "none" }),
      registrar: () => fakeRegistrarAuthority("admin", 2),
    });
    const registrations: Array<{ epoch: bigint; revoke: Uint8Array[] }> = [];
    const out = await client(registrations).sweepIfCommitter(null, g.group, CURSOR, () => {}, ACT, () => {});
    expect(out.kind).toBe("committed");
    expect(registrations).toEqual([{ epoch: 7n, revoke: [] }]);
  });

  it("an admin with no collected key for the evictee registers and revokes nothing", async () => {
    const g = sweepingGroup();
    dsAccess.setAuthorityFactory({
      root: () => ({ kind: "none" }),
      registrar: () => fakeRegistrarAuthority("admin", 2),
    });
    const registrations: Array<{ epoch: bigint; revoke: Uint8Array[] }> = [];
    const out = await client(registrations).sweepIfCommitter(null, g.group, CURSOR, () => {}, ACT, () => {});
    expect(out.kind).toBe("committed");
    expect(registrations).toEqual([{ epoch: 7n, revoke: [] }]);
  });

  it("with neither root nor registrar seed the sweep is not staged", async () => {
    const g = sweepingGroup();
    dsAccess.setAuthorityFactory({ root: () => ({ kind: "none" }), registrar: () => ({ kind: "none" }) });
    const registrations: Array<{ epoch: bigint; revoke: Uint8Array[] }> = [];
    const out = await client(registrations).sweepIfCommitter(null, g.group, CURSOR, () => {}, ACT, () => {});
    expect(out).toEqual({ kind: "no-journal-authority", rank: 0 });
    expect(g.order).toEqual([]);
    expect(registrations).toEqual([]);
  });
});
