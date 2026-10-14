// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The store gate (`account-store-gate.ts`): a write of an account's data that
 * is still in flight when Forget device wipes the account does not put the
 * data back, in this tab or another; the durable MLS writes throw instead of
 * skipping; a plain sign-out closes the session-token row only.
 */
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import {
  AccountStoresSealedError,
  WIPED_MARKER_KEY,
  WIPED_MARKER_TTL_MS,
  accountSessionWritable,
  accountStoresWritable,
  finishAccountWipe,
  installAccountStorageGate,
  openAccountStores,
  pruneWipedMarker,
  requireAccountStoresWritable,
  resetAccountStoreGateForTests,
  sealAccountSession,
  sealAccountStoresForWipe,
  storageKeyWritable,
} from "./account-store-gate";
import { accountStoreIds, wipeAccountLocalData } from "./wipe-account";
import { base64Encode, base64UrlEncodeNoPad } from "./account-id";
import { loadPersistedAccount, savePersistedAccount } from "./persisted-account";
import { getSessionPair, putSessionPair } from "./session-token-store";
import type { SessionPair } from "./session-pair";
import { loadMlsSnapshot, saveMlsSnapshot, saveSnapshotWithCommitEntry } from "../chat/mls-store";
import { putEntryDurable, type PendingCommitEntry } from "../chat/commit-outbox";
import { loadMessageLog, saveMessageLog } from "../chat/message-log-store";
import {
  getDeviceGroupSnapshot,
  putDeviceGroupSnapshot,
} from "../devices/device-group-snapshot-store";

// A's bytes give `+` and `/` in standard base64, so its base64url form differs.
const A_BYTES = new Uint8Array(16).fill(0xfb);
const A = base64Encode(A_BYTES);
const A_URL = base64UrlEncodeNoPad(A_BYTES);
const B = base64Encode(new Uint8Array(16).fill(0x5a));

let local: MemoryStorage;
let idb: FakeIndexedDb;

beforeEach(() => {
  local = new MemoryStorage();
  idb = new FakeIndexedDb();
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = local;
  g["sessionStorage"] = new MemoryStorage();
  g["indexedDB"] = idb;
  resetAccountStoreGateForTests();
});

afterEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["sessionStorage"];
  delete g["indexedDB"];
  resetAccountStoreGateForTests();
});

function deviceKey(): Promise<CryptoKey> {
  return crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, [
    "encrypt",
    "decrypt",
  ]);
}

/** A second tab: its own memory, the same `localStorage`. */
function asAnotherTab(): void {
  resetAccountStoreGateForTests();
}

describe("the gate", () => {
  it("is open for an account nobody sealed", () => {
    expect(accountStoresWritable(A)).toBe(true);
    expect(() => requireAccountStoresWritable(A)).not.toThrow();
    expect(accountSessionWritable(A)).toBe(true);
  });

  it("a wipe seals every form of the account, and only that account", () => {
    sealAccountStoresForWipe(accountStoreIds(A));
    expect(accountStoresWritable(A)).toBe(false);
    expect(accountStoresWritable(A_URL)).toBe(false);
    expect(accountSessionWritable(A)).toBe(false);
    expect(accountStoresWritable(B)).toBe(true);
    expect(() => requireAccountStoresWritable(A)).toThrow(AccountStoresSealedError);
  });

  it("stays sealed after the wipe until the account lands again", () => {
    const ids = accountStoreIds(A);
    sealAccountStoresForWipe(ids);
    finishAccountWipe(ids);
    expect(accountStoresWritable(A)).toBe(false);
    openAccountStores(ids);
    expect(accountStoresWritable(A)).toBe(true);
    expect(accountStoresWritable(A_URL)).toBe(true);
  });

  it("a landing that overlaps the wipe does not open the account", () => {
    const ids = accountStoreIds(A);
    sealAccountStoresForWipe(ids);
    openAccountStores(ids);
    expect(accountStoresWritable(A)).toBe(false);
    finishAccountWipe(ids);
    openAccountStores(ids);
    expect(accountStoresWritable(A)).toBe(true);
  });

  it("holds in another tab, which reads the marker at the moment it writes", () => {
    sealAccountStoresForWipe(accountStoreIds(A));
    asAnotherTab();
    expect(accountStoresWritable(A)).toBe(false);
    expect(accountStoresWritable(B)).toBe(true);
    // That tab keeps it sealed once the marker is gone.
    local.removeItem(WIPED_MARKER_KEY);
    expect(accountStoresWritable(A)).toBe(false);
  });

  it("two tabs: a wipe in one seals the other, which stays sealed when the first lands again", async () => {
    // A second instance of the module over the same `localStorage` is a
    // second tab: its own memory, the shared marker.
    vi.resetModules();
    const tab2 = await import("./account-store-gate");
    const ids = accountStoreIds(A);
    // Tab 2 is signed in to the account.
    tab2.openAccountStores(ids);
    expect(tab2.accountStoresWritable(A)).toBe(true);
    // Tab 1 forgets the device.
    sealAccountStoresForWipe(ids);
    finishAccountWipe(ids);
    expect(tab2.accountStoresWritable(A)).toBe(false);
    expect(tab2.storageKeyWritable(`lakko.contacts.v3.${A}`)).toBe(false);
    expect(tab2.storageKeyWritable(`lakko.invite-link-secrets.v1.${A_URL}`)).toBe(false);
    expect(tab2.accountStoresWritable(B)).toBe(true);
    // Tab 1 signs in to the account again: open there, the marker entry gone,
    // and tab 2 (whose session is the forgotten one) still sealed.
    openAccountStores(ids);
    expect(local.getItem(WIPED_MARKER_KEY)).toBeNull();
    expect(accountStoresWritable(A)).toBe(true);
    expect(tab2.accountStoresWritable(A)).toBe(false);
    // Until tab 2 lands the account itself.
    tab2.openAccountStores(ids);
    expect(tab2.accountStoresWritable(A)).toBe(true);
  });

  it("the marker names no account and is pruned after its lifetime", () => {
    sealAccountStoresForWipe(accountStoreIds(A), 1_000);
    const marker = local.getItem(WIPED_MARKER_KEY);
    expect(marker).not.toBeNull();
    for (const text of [WIPED_MARKER_KEY, marker ?? ""]) {
      expect(text).not.toContain(A);
      expect(text).not.toContain(A_URL);
    }
    pruneWipedMarker(1_000 + WIPED_MARKER_TTL_MS - 1);
    expect(local.getItem(WIPED_MARKER_KEY)).not.toBeNull();
    pruneWipedMarker(1_000 + WIPED_MARKER_TTL_MS);
    expect(local.getItem(WIPED_MARKER_KEY)).toBeNull();
    // Pruning the marker does not open the account in the wiping tab.
    expect(accountStoresWritable(A)).toBe(false);
  });

  it("a damaged marker reads as empty", () => {
    local.setItem(WIPED_MARKER_KEY, "{not json");
    expect(accountStoresWritable(A)).toBe(true);
    local.setItem(WIPED_MARKER_KEY, JSON.stringify([{ h: 7 }, null, "x"]));
    expect(accountStoresWritable(A)).toBe(true);
  });

  it("a sign-out closes the session-token row and nothing else", () => {
    sealAccountSession(A);
    expect(accountSessionWritable(A)).toBe(false);
    expect(accountStoresWritable(A)).toBe(true);
    expect(() => requireAccountStoresWritable(A)).not.toThrow();
    // Not across tabs: another tab's session is its own.
    asAnotherTab();
    expect(accountSessionWritable(A)).toBe(true);
  });

  it("a sign-out's seal ends at the account's next landing", () => {
    sealAccountSession(A);
    openAccountStores(accountStoreIds(A));
    expect(accountSessionWritable(A)).toBe(true);
  });
});

describe("localStorage and sessionStorage", () => {
  it("a key that names a sealed account is not writable", () => {
    sealAccountStoresForWipe(accountStoreIds(A));
    expect(storageKeyWritable(`lakko.contacts.v3.${A}`)).toBe(false);
    expect(storageKeyWritable(`lakko.invite-link-secrets.v1.${A_URL}`)).toBe(false);
    expect(storageKeyWritable(`lakko.contacts.v3.${B}`)).toBe(true);
    expect(storageKeyWritable("lakko.theme")).toBe(true);
    expect(storageKeyWritable(WIPED_MARKER_KEY)).toBe(true);
  });

  it("the installed gate drops a late write for the wiped account, in every Storage", () => {
    // The host has no `Storage`; give it one the way a browser has it.
    class Storage {
      readonly map = new Map<string, string>();
      getItem(key: string): string | null {
        return this.map.get(key) ?? null;
      }
      setItem(key: string, value: string): void {
        this.map.set(key, value);
      }
      removeItem(key: string): void {
        this.map.delete(key);
      }
    }
    const g = globalThis as unknown as Record<string, unknown>;
    g["Storage"] = Storage;
    const store = new Storage();
    g["localStorage"] = store;
    const uninstall = installAccountStorageGate();
    try {
      store.setItem(`lakko.contacts.v3.${A}`, "before");
      sealAccountStoresForWipe(accountStoreIds(A));
      store.removeItem(`lakko.contacts.v3.${A}`);
      // The write that was suspended across the wipe:
      store.setItem(`lakko.contacts.v3.${A}`, "after");
      store.setItem(`lakko.contacts.v3.${B}`, "other account");
      expect(store.getItem(`lakko.contacts.v3.${A}`)).toBeNull();
      expect(store.getItem(`lakko.contacts.v3.${B}`)).toBe("other account");
      // The gate's own marker went through.
      expect(store.getItem(WIPED_MARKER_KEY)).not.toBeNull();
      finishAccountWipe(accountStoreIds(A));
      openAccountStores(accountStoreIds(A));
      store.setItem(`lakko.contacts.v3.${A}`, "landed again");
      expect(store.getItem(`lakko.contacts.v3.${A}`)).toBe("landed again");
    } finally {
      uninstall();
      delete g["Storage"];
    }
    store.setItem(`x.${A}`, "uninstalled");
    expect(store.getItem(`x.${A}`)).toBe("uninstalled");
  });

  it("the cached-account marker is not written for a wiped account", async () => {
    savePersistedAccount({ v: 1, accountIdB64: A, username: "alice" });
    await wipeAccountLocalData(A);
    expect(loadPersistedAccount()).toBeNull();
    savePersistedAccount({ v: 1, accountIdB64: A, username: "alice" });
    expect(loadPersistedAccount()).toBeNull();
    savePersistedAccount({ v: 1, accountIdB64: B, username: "bob" });
    expect(loadPersistedAccount()?.accountIdB64).toBe(B);
  });
});

describe("a write in flight when Forget device wipes the account", () => {
  it("the MLS snapshot is not put back, and the write throws", async () => {
    const key = await deviceKey();
    await saveMlsSnapshot(A, key, new Uint8Array([1, 2, 3]), ["g"]);
    expect(await loadMlsSnapshot(A, key)).not.toBeNull();
    // Started before the wipe, suspended on its encrypt while the wipe runs.
    const late = saveMlsSnapshot(A, key, new Uint8Array([4, 5, 6]), ["g"]);
    const outcome = late.then(
      () => null,
      (err: unknown) => err,
    );
    await wipeAccountLocalData(A);
    expect(await outcome).toBeInstanceOf(AccountStoresSealedError);
    expect(await loadMlsSnapshot(A, key)).toBeNull();
    // And a write that starts after the wipe.
    await expect(saveMlsSnapshot(A, key, new Uint8Array([7]), [])).rejects.toBeInstanceOf(
      AccountStoresSealedError,
    );
    expect(await loadMlsSnapshot(A, key)).toBeNull();
  });

  it("the snapshot-with-commit-entry write and the outbox write throw too", async () => {
    const key = await deviceKey();
    const entry = { id: `${A}#1`, accountId: A, acked: false } as unknown as PendingCommitEntry;
    await wipeAccountLocalData(A);
    await expect(
      saveSnapshotWithCommitEntry(A, key, new Uint8Array([1]), [], entry),
    ).rejects.toBeInstanceOf(AccountStoresSealedError);
    await expect(putEntryDurable(entry)).rejects.toBeInstanceOf(AccountStoresSealedError);
    const outbox = idb.dbs.get("lakko-mls")?.stores.get("commit-outbox");
    expect(outbox === undefined ? 0 : outbox.rows.size).toBe(0);
  });

  it("the DeviceGroup snapshot write throws", async () => {
    const key = await deviceKey();
    const late = putDeviceGroupSnapshot(A, new Uint8Array([9, 9]), key).then(
      () => null,
      (err: unknown) => err,
    );
    await wipeAccountLocalData(A);
    expect(await late).toBeInstanceOf(AccountStoresSealedError);
    expect(await getDeviceGroupSnapshot(A, key)).toBeNull();
  });

  it("the message log is not put back, and the write is a silent skip", async () => {
    const key = await deviceKey();
    await saveMessageLog(A, key, { version: 1, groups: {} });
    expect(await loadMessageLog(A, key)).not.toBeNull();
    const late = saveMessageLog(A, key, { version: 1, groups: {} });
    await wipeAccountLocalData(A);
    await expect(late).resolves.toBeUndefined();
    expect(await loadMessageLog(A, key)).toBeNull();
  });

  it("another account's writes go on through the wipe", async () => {
    const key = await deviceKey();
    const other = saveMessageLog(B, key, { version: 1, groups: {} });
    const otherMls = saveMlsSnapshot(B, key, new Uint8Array([1]), []);
    await wipeAccountLocalData(A);
    await other;
    await otherMls;
    expect(await loadMessageLog(B, key)).not.toBeNull();
    expect(await loadMlsSnapshot(B, key)).not.toBeNull();
  });

  it("the account's stores accept writes again once it lands", async () => {
    const key = await deviceKey();
    await wipeAccountLocalData(A);
    openAccountStores(accountStoreIds(A));
    await saveMlsSnapshot(A, key, new Uint8Array([1, 2]), []);
    await saveMessageLog(A, key, { version: 1, groups: {} });
    expect(await loadMlsSnapshot(A, key)).not.toBeNull();
    expect(await loadMessageLog(A, key)).not.toBeNull();
  });

  it("a wipe in another tab refuses this tab's writes", async () => {
    const key = await deviceKey();
    await wipeAccountLocalData(A);
    asAnotherTab();
    await expect(saveMlsSnapshot(A, key, new Uint8Array([1]), [])).rejects.toBeInstanceOf(
      AccountStoresSealedError,
    );
    await saveMessageLog(A, key, { version: 1, groups: {} });
    expect(await loadMessageLog(A, key)).toBeNull();
  });
});

describe("a session token write in flight at sign-out", () => {
  it("does not bring the token back", async () => {
    const key = await deviceKey();
    const pair = (token: string): SessionPair => ({
      sessionToken: token,
      sessionExpiresAtMs: Date.now() + 60_000,
      refreshToken: `r.${token}`,
      refreshExpiresAtMs: Date.now() + 120_000,
      keyGeneration: 1n,
    });
    await putSessionPair(A, pair("v3.tok"), key);
    expect(await getSessionPair(A, key)).not.toBeNull();
    // `logout`: seal, then delete; the late persist of a sign-in follows.
    sealAccountSession(A);
    const { deleteSessionToken } = await import("./session-token-store");
    await deleteSessionToken(A);
    await putSessionPair(A, pair("v3.late"), key);
    expect(await getSessionPair(A, key)).toBeNull();
    // The account's next landing opens the row again.
    openAccountStores(accountStoreIds(A));
    await putSessionPair(A, pair("v3.new"), key);
    expect((await getSessionPair(A, key))?.sessionToken).toBe("v3.new");
  });
});

// Every store asks the gate at the point the write is issued; that it does,
// and that a store added later is noticed, is pinned on the sources.
describe("store wiring", () => {
  const src = fileURLToPath(new URL("..", import.meta.url));

  function sources(dir: string, out: string[] = []): string[] {
    for (const name of readdirSync(dir)) {
      const path = join(dir, name);
      if (statSync(path).isDirectory()) {
        if (name !== "test") sources(path, out);
      } else if (/\.tsx?$/.test(name) && !/\.test\.tsx?$/.test(name)) {
        out.push(path);
      }
    }
    return out;
  }

  /** IndexedDB writers that hold no account's data. */
  const DEVICE_WIDE = new Set<string>([]);

  it("every module that opens a read-write transaction asks the gate", () => {
    const unasked: string[] = [];
    for (const path of sources(src)) {
      const text = readFileSync(path, "utf8");
      if (!/transaction\([^)]*"readwrite"\)/.test(text)) continue;
      const rel = path.slice(src.length);
      if (DEVICE_WIDE.has(rel)) continue;
      if (!/(accountStoresWritable|requireAccountStoresWritable|accountSessionWritable)\(/.test(text)) {
        unasked.push(rel);
      }
    }
    expect(unasked).toEqual([]);
  });

  it("the gate is asked on the line before the transaction is created", () => {
    // No `await` may separate the check from the transaction (the module
    // doc): the three durable writes and the message log, by their text.
    const read = (rel: string): string => readFileSync(join(src, rel), "utf8");
    expect(read("chat/mls-store.ts")).toMatch(
      /requireAccountStoresWritable\(accountIdB64\);\s*const tx = db\.transaction\(STORE, "readwrite"\);/,
    );
    expect(read("chat/mls-store.ts")).toMatch(
      /requireAccountStoresWritable\(accountIdB64\);\s*const tx = db\.transaction\(\[STORE, OUTBOX_STORE\], "readwrite"\);/,
    );
    expect(read("chat/commit-outbox.ts")).toMatch(
      /requireAccountStoresWritable\(entry\.accountId\);\s*const tx = db\.transaction\(OUTBOX_STORE, "readwrite"\);/,
    );
    expect(read("devices/device-group-snapshot-store.ts")).toMatch(
      /requireAccountStoresWritable\(accountIdB64\);\s*await new Promise<void>\(\(resolve, reject\) => \{\s*const tx = db\.transaction\(STORE, "readwrite"\);/,
    );
    expect(read("chat/message-log-store.ts")).toMatch(
      /if \(!accountStoresWritable\(accountIdB64\)\) return;\s*const tx = db\.transaction\(STORE, "readwrite"\);/,
    );
  });

  it("the wipe seals before its first delete, and sign-out seals before its delete", () => {
    const wipe = readFileSync(join(src, "auth/wipe-account.ts"), "utf8");
    expect(wipe.indexOf("sealAccountStoresForWipe(ids);")).toBeGreaterThan(-1);
    expect(wipe.indexOf("sealAccountStoresForWipe(ids);")).toBeLessThan(
      wipe.indexOf("await runWipeSteps(steps, failed);"),
    );
    const auth = readFileSync(join(src, "auth/AuthContext.tsx"), "utf8");
    const logout = auth.slice(auth.indexOf("\n    logout() {"), auth.indexOf("\n    async forgetDevice() {"));
    expect(logout.indexOf("sealAccountSession(")).toBeGreaterThan(-1);
    expect(logout.indexOf("sealAccountSession(")).toBeLessThan(logout.indexOf("deleteSessionToken("));
    expect(auth).toContain("onCleanup(installAccountStorageGate());");
    expect(auth).toMatch(/binding\.landed\(\);\s*openStoresFor\(next\.account\.accountId\);/);
  });
});
