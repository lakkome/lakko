// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/one-tab.md section 6 — the snapshot writer's backstops, against the
 * real wasm core and the real `mls-store.ts`.
 *
 * The defect these pin (seen live 2026-11-21): two realms (tabs) of one
 * browser each held the whole MLS provider in memory and wrote it whole under
 * an account-only key. The realm that was behind wrote its old copy over the
 * newer one; on the next load a group sat below the epoch recorded with its
 * cursor, and the commits above it were this device's own, which it can never
 * apply. Each realm here is its own `SnapshotWriter` and `Identity` over one
 * shared IndexedDB, as two tabs are.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import type { DeviceCipher } from "../auth/device-cipher";
import { resetAccountStoreGateForTests } from "../auth/account-store-gate";
import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import { freezeTab, resetTabStateForTests, TabInactiveError } from "../tab/tab-state";
import { bindContactsAccount } from "../contacts/local-store";
import type { DsClient } from "../ds/client";
import { ChatClient } from "./client";
import { readCursorEpoch, recordCursorEpoch } from "./cursor-epoch";
import {
  awaitTx,
  loadMlsSnapshot,
  openMlsDb,
  SnapshotWriter,
  StaleProviderError,
  type SnapshotCapture,
} from "./mls-store";
import { markOwnPublished } from "./own-published";
import { failedApplyKind, registeredCommitHolds } from "./registered-commit-hold";

const ACCOUNT = "b25lLXRhYi1hY2NvdW50LTAw";
const DEVICE_SEED = new Uint8Array(64).fill(0x41);

/** A transparent cipher (plaintext + a 16-byte tag), with an optional delay
 *  per encrypt so a test can make two writes' encrypts finish out of order. */
function cipher(delays: number[] = []): DeviceCipher {
  return {
    encrypt: async (_iv, pt) => {
      const d = delays.shift() ?? 0;
      if (d > 0) await new Promise((r) => setTimeout(r, d));
      const out = new Uint8Array(pt.length + 16);
      out.set(pt, 0);
      return out.buffer;
    },
    decrypt: async (_iv, ct) => ct.slice(0, ct.length - 16).buffer,
  };
}

const b64 = (b: Uint8Array): string => btoa(String.fromCharCode(...b));
const unb64 = (s: string): Uint8Array => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

beforeEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  g["indexedDB"] = new FakeIndexedDb();
  g["localStorage"] = new MemoryStorage();
  resetAccountStoreGateForTests();
  resetTabStateForTests();
  bindContactsAccount(ACCOUNT);
  vi.spyOn(console, "warn").mockImplementation(() => {});
});
afterEach(() => {
  bindContactsAccount(null);
  resetTabStateForTests();
  vi.restoreAllMocks();
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["indexedDB"];
  delete g["localStorage"];
});

/** One page load: a canonical Identity restored from the shared row, its own
 *  writer adopting the row's generation (what `rehydrate.ts` does). */
async function openRealm(key: DeviceCipher = cipher()) {
  const writer = new SnapshotWriter();
  const identity = wasm.Identity.fromSeed(DEVICE_SEED);
  const snap = await loadMlsSnapshot(ACCOUNT, key);
  writer.adoptLoaded(ACCOUNT, snap?.generation ?? 0);
  if (snap !== null) identity.restoreStorage(snap.snapshotBytes);
  const groups = (snap?.knownGroupIds ?? []).map((gid) => {
    const g = wasm.Group.load(identity, unb64(gid))!;
    writer.noteRestoredEpoch(gid, g.epoch);
    return g;
  });
  const capture = (): SnapshotCapture => ({
    snapshotBytes: identity.snapshotStorage(),
    knownGroupIds: groups.map((g) => b64(g.groupId)),
    leafKeyB64: b64(identity.signaturePublicKey),
    groupEpochs: Object.fromEntries(groups.map((g) => [b64(g.groupId), g.epoch])),
  });
  return {
    writer,
    identity,
    groups,
    capture,
    persist: () => writer.write(ACCOUNT, key, capture),
  };
}

/** This device owns a group with two members; the row is written at its
 *  starting epoch. */
async function founded() {
  const owner = wasm.Identity.fromSeed(DEVICE_SEED);
  const m1 = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x42));
  const m2 = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x43));
  const g = new wasm.Group(owner, wasm.GroupKind.Group);
  g.addMembers(owner, [m1.createKeyPackage(), m2.createKeyPackage()]);
  const gid = b64(g.groupId);
  await new SnapshotWriter().write(ACCOUNT, cipher(), () => ({
    snapshotBytes: owner.snapshotStorage(),
    knownGroupIds: [gid],
    leafKeyB64: b64(owner.signaturePublicKey),
    groupEpochs: { [gid]: g.epoch },
  }));
  recordCursorEpoch(gid, g.epoch);
  const leafOf = (who: wasm.Identity): number =>
    (g.members() as Array<{ leafIndex: number; signaturePublicKey: Uint8Array }>).find(
      (m) => b64(m.signaturePublicKey) === b64(who.signaturePublicKey),
    )!.leafIndex;
  return { gid, startEpoch: g.epoch, m1, leafM2: leafOf(m2) };
}

/** The ADR-0045 sweep's MLS half: own proposal, stage, merge. */
function sweepCommit(id: wasm.Identity, g: wasm.Group, propose: () => void): Uint8Array {
  propose();
  const staged = g.stagePendingProposals(id)!;
  const commit = staged.commit;
  g.mergeStagedSweep(id);
  return commit;
}

describe("two realms of one browser cannot both write", () => {
  it("the realm that is behind is refused; the newer row stays and loads", async () => {
    const { gid, startEpoch, m1, leafM2 } = await founded();
    const tabA = await openRealm();
    const tabB = await openRealm(); // open before A's commits, never reloads
    const gA = tabA.groups[0]!;
    const stale = vi.fn();
    tabB.writer.onStale(stale);

    const c1 = sweepCommit(tabA.identity, gA, () => gA.proposeRemoveMember(tabA.identity, leafM2));
    await tabA.persist();
    recordCursorEpoch(gid, gA.epoch);
    sweepCommit(tabA.identity, gA, () =>
      gA.proposeGroupRoles(tabA.identity, tabA.identity.signaturePublicKey, m1.signaturePublicKey),
    );
    await tabA.persist();
    recordCursorEpoch(gid, gA.epoch);

    // Before the fix this write put the group back at startEpoch.
    await expect(tabB.persist()).rejects.toBeInstanceOf(StaleProviderError);
    expect(stale).toHaveBeenCalledTimes(1);

    const reload = await openRealm();
    expect(reload.groups[0]!.epoch).toBe(startEpoch + 2n);
    expect(readCursorEpoch(gid)).toBe(startEpoch + 2n);

    // Control: what the refused write would have left behind is unrecoverable
    // for this device (its own commit), which is why it must never land.
    let err: unknown;
    try {
      tabB.groups[0]!.processIncomingVerifyingRoles(tabB.identity, c1);
    } catch (e) {
      err = e;
    }
    expect(failedApplyKind(err)).toBe("own-message");
  });

  it("same epoch, other ratchet: a realm whose row was rewritten by another is refused (generation)", async () => {
    await founded();
    const tabA = await openRealm();
    const tabB = await openRealm();
    // A sends (the ratchet moves, the epoch does not) and persists.
    tabA.groups[0]!.createApplicationMessage(tabA.identity, new TextEncoder().encode("hi"));
    await tabA.persist();
    await expect(tabB.persist()).rejects.toBeInstanceOf(StaleProviderError);
  });

  it("a writer that never restored the row is still refused when it would lower a group's epoch", async () => {
    const { leafM2 } = await founded();
    const tabA = await openRealm();
    const stale = await openRealm();
    const gA = tabA.groups[0]!;
    sweepCommit(tabA.identity, gA, () => gA.proposeRemoveMember(tabA.identity, leafM2));
    await tabA.persist();
    const fresh = new SnapshotWriter(); // no adoptLoaded: not bound by generation
    await expect(fresh.write(ACCOUNT, cipher(), stale.capture)).rejects.toThrow(/epoch/);
  });

  it("the stale realm neither persists nor publishes a message", async () => {
    const { leafM2 } = await founded();
    const tabA = await openRealm();
    const tabB = await openRealm();
    const gA = tabA.groups[0]!;
    sweepCommit(tabA.identity, gA, () => gA.proposeRemoveMember(tabA.identity, leafM2));
    await tabA.persist();

    const publishGroup = vi.fn(async () => ({ id: 1, createdAt: "2026-11-21T00:00:00Z" }));
    const chatB = new ChatClient({ publishGroup } as unknown as DsClient, {
      identityProvider: () => tabB.identity,
      onPersist: () => tabB.persist(),
    });
    await expect(chatB.sendMessage(null, tabB.groups[0]!, new TextEncoder().encode("late"))).rejects.toThrow();
    expect(publishGroup).not.toHaveBeenCalled();
    expect((await openRealm()).groups[0]!.epoch).toBe(gA.epoch);
  });

  it("a frozen page writes nothing", async () => {
    await founded();
    const tab = await openRealm();
    freezeTab();
    await expect(tab.persist()).rejects.toBeInstanceOf(TabInactiveError);
  });
});

describe("one realm: writes land in the order their snapshots were taken (c2)", () => {
  it("an older capture whose encrypt is slow cannot land after a newer one", async () => {
    const { gid, startEpoch, leafM2 } = await founded();
    // Encrypts in call order: P1 30 ms, P2 0 ms.
    const key = cipher([30, 0]);
    const tab = await openRealm();
    const g = tab.groups[0]!;
    const p1 = tab.writer.write(ACCOUNT, key, tab.capture);
    sweepCommit(tab.identity, g, () => g.proposeRemoveMember(tab.identity, leafM2));
    const p2 = tab.writer.write(ACCOUNT, key, tab.capture);
    await Promise.all([p1, p2]);
    const reload = await openRealm();
    expect(reload.groups[0]!.epoch).toBe(startEpoch + 1n);
    expect(tab.writer.durableEpoch(gid)).toBe(startEpoch + 1n);
  });
});

describe("the cursor epoch follows the disk, not memory (c1)", () => {
  function pollFixture(opts: { inMemory: bigint; onDisk: bigint | undefined; persistFails?: boolean }) {
    const groupId = new Uint8Array([0x7e, Number(opts.onDisk ?? 0n) + 1]);
    const gid = b64(groupId);
    let persists = 0;
    const group = {
      get groupId() {
        return groupId;
      },
      get epoch() {
        return opts.inMemory;
      },
      members: () => [],
      hasStagedSweep: () => false,
      roleTableOwner: () => undefined,
      ownCommitterRank: () => undefined,
      processIncomingVerifyingRoles: () => {
        throw new Error("never reached: the only row is our own echo");
      },
    } as unknown as wasm.Group;
    const ds = {
      fetchGroup: async () => ({
        messages: [{ id: 160, payload: new Uint8Array([1]), createdAt: "2026-11-21T10:00:00Z" }],
        hasMore: false,
        latestEpoch: Number(opts.inMemory),
        registeredCommits: new Map<bigint, number>([[opts.inMemory, 160]]),
      }),
    } as unknown as DsClient;
    const chat = new ChatClient(ds, {
      identityProvider: () => ({}) as unknown as wasm.Identity,
      onPersist: async () => {
        persists += 1;
        if (opts.persistFails === true) throw new Error("IndexedDB quota");
      },
    });
    markOwnPublished(gid, 160);
    return { gid, group, chat, persists: () => persists };
  }

  it("a batch of only our own commit persists first when memory is past the disk, and records the disk", async () => {
    const { mlsSnapshotWriter } = await import("./mls-store");
    const f = pollFixture({ inMemory: 78n, onDisk: 77n });
    mlsSnapshotWriter.noteRestoredEpoch(f.gid, 77n);
    let cursor = 150;
    await f.chat.pollMessages(null, f.group, cursor, 0, { read: () => cursor, advance: (to) => void (cursor = to) });
    expect(f.persists()).toBe(1);
    expect(cursor).toBe(160);
    // The hook here is not the store's writer, so the disk is still at 77:
    // that, never the in-memory 78, is what is recorded.
    expect(readCursorEpoch(f.gid)).toBe(77n);
  });

  it("when that persist fails the cursor holds and nothing is recorded", async () => {
    const { mlsSnapshotWriter } = await import("./mls-store");
    const f = pollFixture({ inMemory: 78n, onDisk: 76n, persistFails: true });
    mlsSnapshotWriter.noteRestoredEpoch(f.gid, 76n);
    let cursor = 150;
    await f.chat.pollMessages(null, f.group, cursor, 0, { read: () => cursor, advance: (to) => void (cursor = to) });
    expect(cursor).toBe(150);
    expect(readCursorEpoch(f.gid)).toBeUndefined();
  });

  it("a batch that touched nothing records the disk epoch without a write", async () => {
    const { mlsSnapshotWriter } = await import("./mls-store");
    const f = pollFixture({ inMemory: 79n, onDisk: 79n });
    mlsSnapshotWriter.noteRestoredEpoch(f.gid, 79n);
    let cursor = 150;
    await f.chat.pollMessages(null, f.group, cursor, 0, { read: () => cursor, advance: (to) => void (cursor = to) });
    expect(f.persists()).toBe(0);
    expect(readCursorEpoch(f.gid)).toBe(79n);
  });
});

/** The stored row as it is, or put back as it was (raw, no writer). */
async function rawRow(): Promise<Record<string, unknown> | undefined> {
  const db = await openMlsDb();
  try {
    const tx = db.transaction("snapshots", "readonly");
    const req = tx.objectStore("snapshots").get(ACCOUNT);
    return await new Promise((res, rej) => {
      req.onsuccess = (): void => res(req.result as Record<string, unknown> | undefined);
      req.onerror = (): void => rej(req.error);
    });
  } finally {
    db.close();
  }
}
async function putRawRow(row: Record<string, unknown>): Promise<void> {
  const db = await openMlsDb();
  try {
    const tx = db.transaction("snapshots", "readwrite");
    tx.objectStore("snapshots").put(row);
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
}

/**
 * The live sequence of 2026-11-21 (test.lakko.me): the owner's row loads with
 * the group at N, no `generation` field (written by a build before the
 * writer), and the cursor recorded at N+2. The poll reads again from the
 * commit that leaves N, which is this device's own and cannot apply: held,
 * attempt 1, then attempt 2, each with a persist through the writer.
 */
describe("one page, a group behind its cursor (live 2026-11-21)", () => {
  async function behindOwner() {
    const { gid, startEpoch, m1, leafM2 } = await founded();
    const atN = await rawRow(); // the row with the group at N
    const tabA = await openRealm();
    const gA = tabA.groups[0]!;
    const c1 = sweepCommit(tabA.identity, gA, () => gA.proposeRemoveMember(tabA.identity, leafM2));
    const c2 = sweepCommit(tabA.identity, gA, () =>
      gA.proposeGroupRoles(tabA.identity, tabA.identity.signaturePublicKey, m1.signaturePublicKey),
    );
    recordCursorEpoch(gid, startEpoch + 2n);
    // What the older build left on disk: N, and no generation.
    const legacy = { ...atN! };
    delete legacy["generation"];
    delete legacy["groupEpochs"];
    await putRawRow(legacy);
    const page = await openRealm();
    expect(page.groups[0]!.epoch).toBe(startEpoch);
    const ds = {
      fetchGroup: async () => ({
        messages: [
          { id: 101, payload: c1, createdAt: "2026-11-21T10:00:00Z" },
          { id: 102, payload: c2, createdAt: "2026-11-21T10:00:01Z" },
        ],
        hasMore: false,
        latestEpoch: Number(startEpoch + 2n),
        registeredCommits: new Map<bigint, number>([
          [startEpoch + 1n, 101],
          [startEpoch + 2n, 102],
        ]),
      }),
    } as unknown as DsClient;
    const chat = new ChatClient(ds, {
      identityProvider: () => page.identity,
      onPersist: () => page.persist(),
    });
    return { gid, page, chat };
  }

  afterEach(() => {
    registeredCommitHolds.clear();
  });

  it("the hold and its retries write through the writer and are never refused as stale", async () => {
    const { gid, page, chat } = await behindOwner();
    const stale = vi.fn();
    page.writer.onStale(stale);
    let cursor = 102;
    const ref = { read: () => cursor, advance: (to: number) => void (cursor = to) };
    let clock = Date.now();
    vi.spyOn(Date, "now").mockImplementation(() => clock);

    for (let attempt = 1; attempt <= 3; attempt += 1) {
      await chat.pollMessages(null, page.groups[0]!, cursor, 0, ref);
      clock += 60_000; // past the next retry
    }
    expect(registeredCommitHolds.heldRowId(gid)).toBe(101);
    expect(stale).not.toHaveBeenCalled();
    // Every write of the page landed: the row now carries a generation.
    expect(((await rawRow())?.["generation"] as number) >= 1).toBe(true);
    await expect(page.persist()).resolves.toBeUndefined();
    expect(stale).not.toHaveBeenCalled();
  });

  it("what the live page met: an older build writes the row between two writes, and only then is the page refused", async () => {
    const { page, chat } = await behindOwner();
    const stale = vi.fn();
    page.writer.onStale(stale);
    let cursor = 102;
    await chat.pollMessages(null, page.groups[0]!, cursor, 0, {
      read: () => cursor,
      advance: (to) => void (cursor = to),
    });
    await page.persist();
    expect(stale).not.toHaveBeenCalled();
    // The older build has no generation field and no lock: it puts its own
    // copy back.
    const foreign = { ...(await rawRow())! };
    delete foreign["generation"];
    delete foreign["groupEpochs"];
    await putRawRow(foreign);
    await expect(page.persist()).rejects.toBeInstanceOf(StaleProviderError);
    expect(stale).toHaveBeenCalledTimes(1);
  });
});
