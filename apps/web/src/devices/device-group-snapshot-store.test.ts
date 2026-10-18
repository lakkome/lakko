// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0081 decisions 2 and 3 on the web store: the generation that refuses a
 * stale handle's write, the pending rows beside the snapshot, and Forget
 * device removing the outbox with the snapshot under the ADR-0077 gate.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import {
  AccountStoresSealedError,
  resetAccountStoreGateForTests,
  sealAccountStoresForWipe,
} from "../auth/account-store-gate";
import { accountStoreIds } from "../auth/wipe-account";
import {
  DeviceGroupSnapshotChangedError,
  deleteDeviceGroupSnapshot,
  dropDeviceGroupStateForRejoin,
  getDeviceGroupJournalCursor,
  getDeviceGroupPending,
  getDeviceGroupSnapshotState,
  putDeviceGroupPending,
  putDeviceGroupSnapshot,
} from "./device-group-snapshot-store";

const A = "QUFBQUFBQUFBQUFBQUFBQQ==";

beforeEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = new MemoryStorage();
  g["indexedDB"] = new FakeIndexedDb();
  resetAccountStoreGateForTests();
});

afterEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["indexedDB"];
  resetAccountStoreGateForTests();
});

function deviceKey(): Promise<CryptoKey> {
  return crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
}

const bytes = (s: string): Uint8Array => new TextEncoder().encode(s);

describe("ADR-0081 decision 3 - every write bumps the generation, and a stale handle's write is refused", () => {
  it("a conditional write lands only on the generation it read", async () => {
    const key = await deviceKey();
    const g1 = await putDeviceGroupSnapshot(A, bytes("epoch N"), key, 3);
    const read = await getDeviceGroupSnapshotState(A, key);
    expect(read?.generation).toBe(g1);
    // Another handle made the commit's N+1 current.
    const g2 = await putDeviceGroupSnapshot(A, bytes("epoch N+1"), key);
    expect(g2).toBe(g1 + 1);
    // The long-lived handle, still at N, tries to persist a batch.
    await expect(putDeviceGroupSnapshot(A, bytes("epoch N again"), key, 9, g1)).rejects.toBeInstanceOf(
      DeviceGroupSnapshotChangedError,
    );
    const after = await getDeviceGroupSnapshotState(A, key);
    expect(new TextDecoder().decode(after?.snapshot)).toBe("epoch N+1");
    // The cursor of the refused write was not recorded either.
    expect(await getDeviceGroupJournalCursor(A)).toBe(3);
    // On the current generation it lands.
    await putDeviceGroupSnapshot(A, bytes("epoch N+1, batch"), key, 9, g2);
    expect(await getDeviceGroupJournalCursor(A)).toBe(9);
  });
});

describe("ADR-0081 decision 2 - the outbox sits beside the snapshot", () => {
  it("a pending row never replaces the snapshot", async () => {
    const key = await deviceKey();
    await putDeviceGroupSnapshot(A, bytes("epoch N"), key);
    await putDeviceGroupPending(A, "commit", bytes("entry with the N+1 snapshot"), key);
    const snap = await getDeviceGroupSnapshotState(A, key);
    expect(new TextDecoder().decode(snap?.snapshot)).toBe("epoch N");
    expect(new TextDecoder().decode((await getDeviceGroupPending(A, "commit", key)) ?? undefined)).toBe(
      "entry with the N+1 snapshot",
    );
  });

  it("a rejoin drops the snapshot, the in-flight commit and the admission record, and keeps the knock", async () => {
    const key = await deviceKey();
    await putDeviceGroupSnapshot(A, bytes("behind"), key);
    await putDeviceGroupPending(A, "commit", bytes("c"), key);
    await putDeviceGroupPending(A, "admissions", bytes("a"), key);
    await putDeviceGroupPending(A, "rejoin", bytes("r"), key);
    await dropDeviceGroupStateForRejoin(A);
    expect(await getDeviceGroupSnapshotState(A, key)).toBeNull();
    expect(await getDeviceGroupPending(A, "commit", key)).toBeNull();
    expect(await getDeviceGroupPending(A, "admissions", key)).toBeNull();
    expect(await getDeviceGroupPending(A, "rejoin", key)).not.toBeNull();
  });

  it("Forget device removes the outbox with the snapshot; once sealed nothing is written back", async () => {
    const key = await deviceKey();
    await putDeviceGroupSnapshot(A, bytes("s"), key);
    await putDeviceGroupPending(A, "commit", bytes("c"), key);
    await putDeviceGroupPending(A, "rejoin", bytes("r"), key);
    sealAccountStoresForWipe(accountStoreIds(A));
    await deleteDeviceGroupSnapshot(A);
    expect(await getDeviceGroupPending(A, "commit", key)).toBeNull();
    expect(await getDeviceGroupPending(A, "rejoin", key)).toBeNull();
    // A late outbox write of the forgotten account is refused (ADR-0077).
    await expect(putDeviceGroupPending(A, "commit", bytes("late"), key)).rejects.toBeInstanceOf(
      AccountStoresSealedError,
    );
    await expect(putDeviceGroupSnapshot(A, bytes("late"), key)).rejects.toBeInstanceOf(AccountStoresSealedError);
  });
});
