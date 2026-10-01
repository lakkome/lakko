// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Log out keeps the synced state (2026-09-30).
 *
 * The DeviceGroup sync runtime's teardown used to delete the stored row, and
 * the restored DeviceGroup had already consumed the journal rows that built
 * it, so folders, flags and drafts were gone after a re-login. The teardown
 * now flushes the debounced write through {@link SyncStatePersister} and
 * leaves the row; only the account wipe removes it.
 *
 * Driven here the way `DeviceGroupSyncContext.tsx` drives it: a capture that
 * reads the live state and refuses once the account is unbound or the row's
 * delete generation moved, and a save that encrypts into IndexedDB (the
 * in-memory fake) under a real AES-GCM key. The component itself is not
 * mounted: it needs the auth context, the wasm core and the DS.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { FakeIndexedDb, MemoryStorage } from "../../test/fake-storage";
import { base64Encode } from "../../auth/account-id";
import { wipeAccountLocalData } from "../../auth/wipe-account";
import type { StampedEdit } from "../../chat/folders/folder-model";
import { hasPendingFolderSync } from "./folder-outbox";
import {
  deleteSyncState,
  loadSyncState,
  saveSyncState,
  syncStateGeneration,
} from "./sync-state-idb";
import { SyncStatePersister, type CapturedSyncState } from "./sync-state-persister";
import type { SyncStateRow } from "./sync-state-store";

const ACCOUNT = base64Encode(new Uint8Array(16).fill(0x21));

const edit = (hlc: bigint): StampedEdit => ({ t: "removed", hlc, folderIdHex: "00".repeat(16) });

let deviceKey: CryptoKey;

beforeEach(async () => {
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = new MemoryStorage();
  g["sessionStorage"] = new MemoryStorage();
  g["indexedDB"] = new FakeIndexedDb();
  deviceKey = await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, [
    "encrypt",
    "decrypt",
  ]);
});

afterEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["sessionStorage"];
  delete g["indexedDB"];
});

/** A stand-in for the sync runtime: the live state, the binding, and a
 *  persister wired the way the context wires it. Timers never fire on their
 *  own, so a scheduled write is exactly "queued, debounce not yet run". */
function runtime(): {
  persister: SyncStatePersister;
  state: { synced: Uint8Array; pending: StampedEdit[][] };
  bind: () => void;
  unbind: () => void;
  change: () => void;
  teardown: () => void;
} {
  const state = { synced: new Uint8Array([1]), pending: [] as StampedEdit[][] };
  let bound = false;
  let generation: number | null = null;
  const persister = new SyncStatePersister({
    debounceMs: 500,
    save: async (account, captured) => {
      await saveSyncState(account, deviceKey, captured.row, captured.generation);
    },
    setTimer: () => 1,
    clearTimer: () => undefined,
  });
  const capture = (): CapturedSyncState | null => {
    if (!bound || generation === null || syncStateGeneration(ACCOUNT) !== generation) return null;
    return {
      row: { synced: state.synced.slice(), pending: state.pending.map((b) => [...b]) },
      generation,
    };
  };
  return {
    persister,
    state,
    bind: () => {
      bound = true;
      generation = syncStateGeneration(ACCOUNT);
    },
    unbind: () => {
      bound = false;
      generation = null;
    },
    change: () => persister.schedule(ACCOUNT, capture),
    // The context's teardown: flush first, then unbind.
    teardown: () => {
      void persister.flush();
      bound = false;
      generation = null;
    },
  };
}

describe("Log out (the sync runtime's teardown)", () => {
  it("keeps the stored state and its pending folder edits", async () => {
    const rt = runtime();
    rt.bind();
    rt.state.synced = new Uint8Array([7, 7, 7]);
    rt.state.pending = [[edit(5n)]];
    rt.change();
    await rt.persister.flush();

    rt.teardown();
    await rt.persister.idle();

    const row = await loadSyncState(ACCOUNT, deviceKey);
    expect(row).not.toBeNull();
    expect([...(row as SyncStateRow).synced]).toEqual([7, 7, 7]);
    expect((row as SyncStateRow).pending).toEqual([[edit(5n)]]);
    expect(hasPendingFolderSync((row as SyncStateRow).pending)).toBe(true);
  });

  it("writes a change still waiting for its debounce before it stops", async () => {
    const rt = runtime();
    rt.bind();
    rt.state.synced = new Uint8Array([1, 2]);
    rt.change();
    await rt.persister.flush();

    // A draft typed just before Log out: queued, debounce not yet run.
    rt.state.synced = new Uint8Array([1, 2, 3]);
    rt.state.pending = [[edit(9n)]];
    rt.change();
    expect(rt.persister.hasQueued).toBe(true);

    rt.teardown();
    expect(rt.persister.hasQueued).toBe(false);
    await rt.persister.idle();

    const row = (await loadSyncState(ACCOUNT, deviceKey)) as SyncStateRow;
    expect([...row.synced]).toEqual([1, 2, 3]);
    expect(row.pending).toEqual([[edit(9n)]]);
  });

  it("captures nothing once the account is unbound", async () => {
    const rt = runtime();
    rt.bind();
    rt.change();
    rt.unbind();
    await rt.persister.flush();
    expect(await loadSyncState(ACCOUNT, deviceKey)).toBeNull();
  });

  it("writes in order, so a slow earlier write never lands over a later one", async () => {
    const written: number[] = [];
    let releaseFirst: () => void = () => undefined;
    const firstGate = new Promise<void>((r) => {
      releaseFirst = r;
    });
    const persister = new SyncStatePersister({
      debounceMs: 500,
      save: async (_account, captured) => {
        const n = captured.row.synced[0] ?? -1;
        if (n === 1) await firstGate;
        written.push(n);
      },
      setTimer: () => 1,
      clearTimer: () => undefined,
    });
    const at = (n: number) => (): CapturedSyncState => ({
      row: { synced: new Uint8Array([n]), pending: [] },
      generation: 0,
    });
    persister.schedule(ACCOUNT, at(1));
    void persister.flush();
    persister.schedule(ACCOUNT, at(2));
    const done = persister.flush();
    releaseFirst();
    await done;
    expect(written).toEqual([1, 2]);
  });

  it("writes only the newest of several queued changes", async () => {
    const written: number[] = [];
    const persister = new SyncStatePersister({
      debounceMs: 500,
      save: async (_account, captured) => {
        written.push(captured.row.synced[0] ?? -1);
      },
      setTimer: () => 1,
      clearTimer: () => undefined,
    });
    for (const n of [1, 2, 3]) {
      persister.schedule(ACCOUNT, () => ({
        row: { synced: new Uint8Array([n]), pending: [] },
        generation: 0,
      }));
    }
    await persister.flush();
    expect(written).toEqual([3]);
  });

  it("drops a queued change when the device was removed", async () => {
    const rt = runtime();
    rt.bind();
    rt.change();
    rt.persister.cancel();
    await rt.persister.flush();
    expect(await loadSyncState(ACCOUNT, deviceKey)).toBeNull();
  });
});

describe("Forget device and Delete account (the account wipe)", () => {
  it("still remove the stored state and its pending edits", async () => {
    await saveSyncState(ACCOUNT, deviceKey, {
      synced: new Uint8Array([4, 4]),
      pending: [[edit(3n)]],
    });
    expect(await loadSyncState(ACCOUNT, deviceKey)).not.toBeNull();

    const report = await wipeAccountLocalData(ACCOUNT);
    expect(report.failed).not.toContain("device-sync");
    expect(await loadSyncState(ACCOUNT, deviceKey)).toBeNull();
  });

  it("are not undone by a write captured before them", async () => {
    const rt = runtime();
    rt.bind();
    rt.state.pending = [[edit(1n)]];
    rt.change();

    // Captured, then the wipe deletes the row before the write lands.
    const generation = syncStateGeneration(ACCOUNT);
    await deleteSyncState(ACCOUNT);
    await saveSyncState(
      ACCOUNT,
      deviceKey,
      { synced: new Uint8Array([1]), pending: [[edit(1n)]] },
      generation,
    );
    expect(await loadSyncState(ACCOUNT, deviceKey)).toBeNull();

    // The runtime still bound to the wiped account flushes on its teardown
    // (Forget device flips the session after the wipe): nothing is written.
    rt.teardown();
    await rt.persister.idle();
    expect(await loadSyncState(ACCOUNT, deviceKey)).toBeNull();
  });

  it("leave a later sign-in on the same account free to write again", async () => {
    await deleteSyncState(ACCOUNT);
    const rt = runtime();
    rt.bind();
    rt.state.synced = new Uint8Array([8]);
    rt.change();
    await rt.persister.flush();
    const row = (await loadSyncState(ACCOUNT, deviceKey)) as SyncStateRow;
    expect([...row.synced]).toEqual([8]);
  });
});
