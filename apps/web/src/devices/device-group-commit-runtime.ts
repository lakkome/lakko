// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0081 decision 2 on the web client: the ports of the DeviceGroup commit
 * outbox (`device-group-commit.ts`) over IndexedDB, the DS client and the
 * wasm DeviceGroup, and the one entry point every DeviceGroup commit path
 * goes through ({@link commitDeviceGroupChange}).
 */
import type {
  DeviceGroup as WasmDeviceGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import { ensureDeviceIdentitySeed } from "../auth/device-identity-store";
import { DsClient } from "../ds/client";
import { takeSigner, type ProofSigner } from "../ds/access";
import { isCommitConflict } from "../ds/ownership";
import {
  decodeDeviceGroupCommitEntry,
  encodeDeviceGroupCommitEntry,
  resendOwedDeviceGroupCommit,
  runDeviceGroupCommit,
  withDeviceGroupCommitLock,
  type DeviceGroupCommitEntry,
  type DeviceGroupCommitKind,
  type DeviceGroupCommitOutcome,
  type DeviceGroupCommitPorts,
} from "./device-group-commit";
import {
  deleteDeviceGroupPending,
  getDeviceGroupPending,
  makeDeviceGroupSnapshotCurrent,
  putDeviceGroupPending,
} from "./device-group-snapshot-store";
import { ensureDeviceGroup, type EnsuredDeviceGroup } from "./lifecycle";
import { announceDeviceGroupChangedHere } from "./sync/growth";
import { publishWelcome } from "./welcome-courier";

/** Who the commit is for. */
export interface DeviceGroupCommitAccount {
  readonly accountIdBytes: Uint8Array;
  readonly accountIdB64: string;
  readonly deviceKey: CryptoKey;
}

/** What a commit path hands the outbox once it has built its commit on the
 *  throwaway handle (which is now at the commit's epoch in memory). */
export interface BuiltDeviceGroupCommit {
  readonly kind: DeviceGroupCommitKind;
  /** The journal row: the ADR-0015 envelope around the commit. */
  readonly row: Uint8Array;
  /** An added device's Welcome and the rendezvous it is delivered to. */
  readonly welcome?: { readonly bytes: Uint8Array; readonly rendezvous: Uint8Array };
  /** For an add: the leaves before it (the growth re-send). */
  readonly priorLeaves?: readonly number[];
}

/** The ports of the outbox for one account. */
export function deviceGroupCommitPorts(
  account: DeviceGroupCommitAccount,
  ds: DsClient = new DsClient(),
): DeviceGroupCommitPorts {
  return {
    async saveEntry(entry) {
      await putDeviceGroupPending(
        account.accountIdB64,
        "commit",
        encodeDeviceGroupCommitEntry(entry),
        account.deviceKey,
      );
    },
    async loadEntry() {
      const bytes = await getDeviceGroupPending(account.accountIdB64, "commit", account.deviceKey);
      if (bytes === null) return null;
      const entry = decodeDeviceGroupCommitEntry(bytes);
      if (entry === null) {
        // A row this build cannot read cannot be finished; it is dropped.
        await deleteDeviceGroupPending(account.accountIdB64, "commit");
      }
      return entry;
    },
    async deleteEntry() {
      await deleteDeviceGroupPending(account.accountIdB64, "commit");
    },
    async durableProof() {
      let ensured: EnsuredDeviceGroup;
      try {
        ensured = await loadThrowaway(account);
      } catch {
        return null;
      }
      try {
        return takeSigner(() => ensured.group.dsAccessSigner(ensured.identity));
      } finally {
        freeHandles(ensured.group, ensured.identity);
      }
    },
    async commit(entry, proof) {
      return ds.commitGroupEpoch(
        base64Decode(entry.journalIdB64),
        BigInt(entry.epoch),
        base64Decode(entry.epochKeyB64),
        [],
        base64Decode(entry.rowB64),
        proof,
      );
    },
    async makeCurrent(entry) {
      await makeDeviceGroupSnapshotCurrent(
        account.accountIdB64,
        base64Decode(entry.nextSnapshotB64),
        account.deviceKey,
      );
    },
    announce(entry) {
      try {
        announceDeviceGroupChangedHere(
          entry.priorLeaves !== undefined && entry.priorLeaves.length > 0 && entry.ownLeaf !== undefined
            ? { growth: { priorLeaves: [...entry.priorLeaves], ownLeaf: entry.ownLeaf } }
            : {},
        );
      } catch (err) {
        console.warn("DeviceGroup change announce failed", err instanceof Error ? err.name : "");
      }
    },
    async deliverWelcome(entry) {
      if (entry.welcomeB64 === undefined || entry.welcomeRendezvousB64 === undefined) return;
      await publishWelcome(base64Decode(entry.welcomeRendezvousB64), base64Decode(entry.welcomeB64), {
        ds,
      });
    },
    isConflict: isCommitConflict,
  };
}

/** A throwaway handle over the durable DeviceGroup state (never founds). */
async function loadThrowaway(account: DeviceGroupCommitAccount): Promise<EnsuredDeviceGroup> {
  const seed = await ensureDeviceIdentitySeed(account.accountIdB64, account.deviceKey);
  try {
    return await ensureDeviceGroup(account.accountIdBytes, account.accountIdB64, seed, account.deviceKey);
  } finally {
    seed.fill(0);
  }
}

function freeHandles(group: WasmDeviceGroup, identity: WasmIdentity): void {
  try {
    group.free();
  } catch {
    /* already freed */
  }
  try {
    identity.free();
  } catch {
    /* already freed */
  }
}

/**
 * Step 6 for the account, under the commit lock: finish an entry a previous
 * run left. `null` when there was none. Throws when it could not be finished
 * now (the entry stays).
 */
export async function resendOwedDeviceGroupCommitFor(
  account: DeviceGroupCommitAccount,
  ds?: DsClient,
): Promise<DeviceGroupCommitOutcome | null> {
  return withDeviceGroupCommitLock(account.accountIdB64, () =>
    resendOwedDeviceGroupCommit(deviceGroupCommitPorts(account, ds)),
  );
}

/** What {@link commitDeviceGroupChange} came to. */
export interface DeviceGroupChangeResult<T> {
  readonly outcome: DeviceGroupCommitOutcome;
  /** What `build` returned beyond the commit, and `after`'s value. */
  readonly value: T;
}

/**
 * Every DeviceGroup commit path: under the account's commit lock, finish an
 * owed entry first (a failure there stops this commit: two commits of one
 * device never interleave), load a throwaway handle at the durable epoch N,
 * let `build` create the commit on it, then run the outbox
 * ({@link runDeviceGroupCommit}): the entry beside the N snapshot, one
 * `POST .../commit`, N+1 made current, the Welcome, the entry deleted.
 *
 * `after` runs on the same throwaway handle (now at N+1) only when the
 * commit landed - the pairing flow seals its history, recovery and state
 * bundles there - and before the handle is freed. Its failure does not undo
 * the commit (it is already on the journal); it propagates.
 *
 * Throws when the build refuses, when the entry cannot be written (nothing
 * was sent), and when the DS cannot be reached (the entry stays and is
 * finished later). A lost epoch is not an error: `outcome.kind === "lost"`.
 */
export async function commitDeviceGroupChange<T>(
  account: DeviceGroupCommitAccount,
  build: (ensured: EnsuredDeviceGroup) => BuiltDeviceGroupCommit | null,
  after: (ensured: EnsuredDeviceGroup, outcome: DeviceGroupCommitOutcome) => Promise<T>,
  ds: DsClient = new DsClient(),
): Promise<DeviceGroupChangeResult<T> | null> {
  return withDeviceGroupCommitLock(account.accountIdB64, async () => {
    const ports = deviceGroupCommitPorts(account, ds);
    await resendOwedDeviceGroupCommit(ports);
    const ensured = await loadThrowaway(account);
    let proof: ProofSigner | null = null;
    try {
      proof = takeSigner(() => ensured.group.dsAccessSigner(ensured.identity));
      if (proof === null) throw new Error("ds: the DeviceGroup journal key could not be derived");
      const ownLeaf = ensured.group.ownLeafIndex();
      const built = build(ensured);
      if (built === null) return null;
      const next = takeSigner(() => ensured.group.dsAccessSigner(ensured.identity));
      if (next === null) throw new Error("ds: the next DeviceGroup epoch's key could not be derived");
      let entry: DeviceGroupCommitEntry;
      try {
        entry = {
          v: 1,
          kind: built.kind,
          journalIdB64: base64Encode(ensured.group.groupId()),
          epoch: next.epoch.toString(),
          epochKeyB64: base64Encode(next.verifyingKey()),
          rowB64: base64Encode(built.row),
          nextSnapshotB64: base64Encode(ensured.identity.snapshotStorage()),
          createdAtMs: Date.now(),
          ...(built.welcome === undefined
            ? {}
            : {
                welcomeB64: base64Encode(built.welcome.bytes),
                welcomeRendezvousB64: base64Encode(built.welcome.rendezvous),
              }),
          ...(built.priorLeaves === undefined ? {} : { priorLeaves: [...built.priorLeaves], ownLeaf }),
        };
      } finally {
        next.free();
      }
      const outcome = await runDeviceGroupCommit(entry, proof, ports);
      const value = await after(ensured, outcome);
      return { outcome, value };
    } finally {
      proof?.free();
      freeHandles(ensured.group, ensured.identity);
    }
  });
}
