// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { type DeviceCipher } from "../auth/device-cipher";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { MemoryStorage } from "../test/fake-storage";
import { startAutoBackup } from "./auto-backup-driver";
import { clearBackupRestoreOwed, markBackupRestoreOwed } from "./restore-owed";
import {
  CLEAN_STATE,
  MIN_BACKUP_INTERVAL_MS,
  backupVerdict,
  loadAutoBackupState,
  markBackupDirty,
  markedDirty,
  knownGroupsChanged,
  markedUploaded,
  runDueBackup,
  saveAutoBackupState,
  type AutoBackupDeps,
  ownStateChanged,
} from "./auto-backup";

const ACCOUNT = "YWNjb3VudC1pZA==";

/** `localStorage` is not present in the node environment vitest runs in. */
function installLocalStorage(): Map<string, string> {
  const backing = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (k: string): string | null => backing.get(k) ?? null,
    setItem: (k: string, v: string): void => void backing.set(k, v),
    removeItem: (k: string): void => void backing.delete(k),
  });
  return backing;
}

beforeEach(() => {
  vi.unstubAllGlobals();
  installLocalStorage();
});

describe("backupVerdict", () => {
  it("is clean while nothing has changed", () => {
    expect(backupVerdict({ lastUploadAtMs: 1000, dirtySinceMs: null, lastGroupsKey: null, lastOwnStateKey: null }, 9e9)).toBe("clean");
  });

  it("is due the first time, however recently the account was created", () => {
    expect(backupVerdict({ lastUploadAtMs: 0, dirtySinceMs: 10, lastGroupsKey: null, lastOwnStateKey: null }, 20)).toBe("due");
  });

  it("throttles a second upload inside the interval", () => {
    const state = { lastUploadAtMs: 1_000_000, dirtySinceMs: 1_000_001, lastGroupsKey: null, lastOwnStateKey: null };
    expect(backupVerdict(state, 1_000_000 + MIN_BACKUP_INTERVAL_MS - 1)).toBe("throttled");
  });

  it("is due once the interval has passed", () => {
    const state = { lastUploadAtMs: 1_000_000, dirtySinceMs: 1_000_001, lastGroupsKey: null, lastOwnStateKey: null };
    expect(backupVerdict(state, 1_000_000 + MIN_BACKUP_INTERVAL_MS)).toBe("due");
  });

  it("is due when the clock has moved backwards, rather than wedging off forever", () => {
    // A `lastUploadAtMs` in the future would make every later `now - last`
    // negative, so a naive comparison would suppress backups permanently.
    const state = { lastUploadAtMs: 9_000_000, dirtySinceMs: 1_000, lastGroupsKey: null, lastOwnStateKey: null };
    expect(backupVerdict(state, 1_000_000)).toBe("due");
  });
});

describe("markedDirty", () => {
  it("records when the account first became worth backing up", () => {
    expect(markedDirty(CLEAN_STATE, 500).dirtySinceMs).toBe(500);
  });

  it("does not push the clock forward on every later change", () => {
    // Otherwise a person editing several things in a row would keep resetting
    // the moment the change happened, and the throttle would never be met.
    const first = markedDirty(CLEAN_STATE, 500);
    expect(markedDirty(first, 900)).toBe(first);
  });
});

describe("markedUploaded", () => {
  it("restarts the clock and clears the dirty mark", () => {
    expect(markedUploaded(CLEAN_STATE, 777)).toEqual({
      lastUploadAtMs: 777,
      dirtySinceMs: null,
      lastGroupsKey: null,
      lastOwnStateKey: null,
    });
  });
});

describe("persistence", () => {
  it("round-trips through localStorage", () => {
    saveAutoBackupState(ACCOUNT, { lastUploadAtMs: 5, dirtySinceMs: 7, lastGroupsKey: "g1", lastOwnStateKey: null });
    expect(loadAutoBackupState(ACCOUNT)).toEqual({ lastUploadAtMs: 5, dirtySinceMs: 7, lastGroupsKey: "g1", lastOwnStateKey: null });
  });

  it("answers clean for an unknown account", () => {
    expect(loadAutoBackupState("never-seen")).toEqual(CLEAN_STATE);
  });

  it.each([
    ["not JSON", "{{{"],
    ["not an object", "42"],
    ["null", "null"],
    ["fields of the wrong type", '{"lastUploadAtMs":"x","dirtySinceMs":"y"}'],
  ])("answers clean rather than throwing on %s", (_name, raw) => {
    const backing = installLocalStorage();
    backing.set(`lakko.backup.auto.${ACCOUNT}`, raw);
    expect(loadAutoBackupState(ACCOUNT)).toEqual(CLEAN_STATE);
  });

  it("answers clean when site data is blocked entirely", () => {
    vi.stubGlobal("localStorage", {
      getItem: (): string => {
        throw new Error("blocked");
      },
      setItem: (): void => {
        throw new Error("blocked");
      },
      removeItem: (): void => {},
    });
    expect(loadAutoBackupState(ACCOUNT)).toEqual(CLEAN_STATE);
    // And marking must not throw into the caller's mutation path.
    expect(() => markBackupDirty(ACCOUNT, 1)).not.toThrow();
  });
});

/** Everything present, so a run turns on the verdict alone. `counter.uploads`
 *  is a live handle rather than a copied number: merging it onto the deps with
 *  `Object.assign` would snapshot zero and never move. */
function readyDeps(overrides: Partial<AutoBackupDeps> = {}): {
  deps: AutoBackupDeps;
  counter: { uploads: number };
} {
  const counter = { uploads: 0 };
  const deps: AutoBackupDeps = {
    accountId: () => new Uint8Array(16),
    accountIdB64: () => ACCOUNT,
    ed25519Seed: () => new Uint8Array(32),
    deviceKey: async () => ({}) as unknown as DeviceCipher,
    sessionToken: () => "token",
    upload: async () => {
      counter.uploads += 1;
      return true;
    },
    now: () => 10_000_000,
    ...overrides,
  };
  return { deps, counter };
}

describe("runDueBackup", () => {
  it("does nothing while the account is clean", async () => {
    const { deps, counter } = readyDeps();
    expect(await runDueBackup(deps)).toBe("clean");
    expect(counter.uploads).toBe(0);
  });

  it("uploads a dirty account and records the upload", async () => {
    markBackupDirty(ACCOUNT, 1);
    const { deps, counter } = readyDeps();
    expect(await runDueBackup(deps)).toBe("uploaded");
    expect(counter.uploads).toBe(1);
    expect(loadAutoBackupState(ACCOUNT)).toEqual({
      lastUploadAtMs: 10_000_000,
      dirtySinceMs: null,
      lastGroupsKey: null,
      lastOwnStateKey: null,
    });
  });

  it("hands the upload no MLS snapshot: the account backup carries none", async () => {
    // A provider snapshot belongs to the device that made it (2026-10-02).
    markBackupDirty(ACCOUNT, 1);
    let seen: Record<string, unknown> | null = null;
    const { deps } = readyDeps({
      upload: async (args) => {
        seen = args as unknown as Record<string, unknown>;
        return true;
      },
    });
    await runDueBackup(deps);
    expect(seen).not.toBeNull();
    expect(Object.keys(seen as unknown as Record<string, unknown>).sort()).toEqual([
      "accountId",
      "deviceKey",
      "ed25519Seed",
      "sessionToken",
    ]);
  });

  it("uploads nothing while a restore of the account's backup is still owed", async () => {
    // The server's blob is the one the restore still has to read; an upload
    // composed before it ran would replace it with a nearly empty state.
    markBackupDirty(ACCOUNT, 1);
    let owed = true;
    const { deps, counter } = readyDeps({ restoreOwed: () => owed });
    expect(await runDueBackup(deps)).toBe("not-ready");
    expect(counter.uploads).toBe(0);
    // The dirty mark survives, so the backup goes out once the restore ran.
    expect(loadAutoBackupState(ACCOUNT).dirtySinceMs).toBe(1);
    owed = false;
    expect(await runDueBackup(deps)).toBe("uploaded");
    expect(counter.uploads).toBe(1);
  });

  it("asks about the owed restore for the account it is about to back up", async () => {
    markBackupDirty(ACCOUNT, 1);
    const asked: string[] = [];
    const { deps } = readyDeps({
      restoreOwed: (accountIdB64) => {
        asked.push(accountIdB64);
        return false;
      },
    });
    await runDueBackup(deps);
    expect(asked).toEqual([ACCOUNT]);
  });

  it("does not upload twice inside the throttle window", async () => {
    markBackupDirty(ACCOUNT, 1);
    const { deps, counter } = readyDeps();
    await runDueBackup(deps);
    markBackupDirty(ACCOUNT, 10_000_001);
    expect(await runDueBackup(deps)).toBe("throttled");
    expect(counter.uploads).toBe(1);
  });

  it.each([
    ["no identity", { accountId: (): null => null }],
    ["no seed", { ed25519Seed: (): null => null }],
    ["no session token", { sessionToken: (): null => null }],
    ["an empty session token", { sessionToken: (): string => "" }],
    ["no device key", { deviceKey: async (): Promise<null> => null }],
  ])("reports not-ready and KEEPS the dirty mark with %s", async (_name, override) => {
    markBackupDirty(ACCOUNT, 1);
    const { deps, counter } = readyDeps(override as Partial<AutoBackupDeps>);
    expect(await runDueBackup(deps)).toBe("not-ready");
    expect(counter.uploads).toBe(0);
    // The change that made it dirty must survive, or it is lost silently.
    expect(loadAutoBackupState(ACCOUNT).dirtySinceMs).toBe(1);
  });

  it("keeps the dirty mark when the upload reports failure", async () => {
    markBackupDirty(ACCOUNT, 1);
    const { deps } = readyDeps({ upload: async () => false });
    expect(await runDueBackup(deps)).toBe("failed");
    expect(loadAutoBackupState(ACCOUNT).dirtySinceMs).toBe(1);
  });

  it("keeps the dirty mark when the upload throws", async () => {
    markBackupDirty(ACCOUNT, 1);
    const { deps } = readyDeps({
      upload: async () => {
        throw new Error("offline");
      },
    });
    expect(await runDueBackup(deps)).toBe("failed");
    expect(loadAutoBackupState(ACCOUNT).dirtySinceMs).toBe(1);
  });

  it("retries on the next run after a failure, once due", async () => {
    markBackupDirty(ACCOUNT, 1);
    let attempt = 0;
    const { deps } = readyDeps({
      upload: async () => {
        attempt += 1;
        return attempt > 1;
      },
    });
    expect(await runDueBackup(deps)).toBe("failed");
    expect(await runDueBackup(deps)).toBe("uploaded");
  });
});

describe("knownGroupsChanged", () => {
  it("reports a change the first time it is asked", () => {
    // Nothing here can vouch for what the server already holds, so an unknown
    // set counts as changed rather than as "same as before".
    expect(knownGroupsChanged(ACCOUNT, ["g1"])).toBe(true);
  });

  it("reports no change for the same set", () => {
    knownGroupsChanged(ACCOUNT, ["g1", "g2"]);
    expect(knownGroupsChanged(ACCOUNT, ["g1", "g2"])).toBe(false);
  });

  it("ignores order, which `groups()` does not guarantee", () => {
    knownGroupsChanged(ACCOUNT, ["g1", "g2"]);
    expect(knownGroupsChanged(ACCOUNT, ["g2", "g1"])).toBe(false);
  });

  it("reports a join", () => {
    knownGroupsChanged(ACCOUNT, ["g1"]);
    expect(knownGroupsChanged(ACCOUNT, ["g1", "g2"])).toBe(true);
  });

  it("reports a departure", () => {
    knownGroupsChanged(ACCOUNT, ["g1", "g2"]);
    expect(knownGroupsChanged(ACCOUNT, ["g1"])).toBe(true);
  });

  it("survives a reload, so a launch is not mistaken for a membership change", () => {
    knownGroupsChanged(ACCOUNT, ["g1"]);
    // A fresh module would start with an empty memo; the persisted key is what
    // keeps "back up when membership moves" from becoming "back up per launch".
    expect(loadAutoBackupState(ACCOUNT).lastGroupsKey).toBe("g1");
    expect(knownGroupsChanged(ACCOUNT, ["g1"])).toBe(false);
  });

  it("does not disturb the dirty mark it did not set", () => {
    markBackupDirty(ACCOUNT, 42);
    knownGroupsChanged(ACCOUNT, ["g1"]);
    expect(loadAutoBackupState(ACCOUNT).dirtySinceMs).toBe(42);
  });
});

describe("startAutoBackup (the driver's own wiring)", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  /** Run the driver for two ticks and return what each ended as. No phrase
   *  cache exists here (no IndexedDB), so an upload that is ATTEMPTED ends
   *  `failed`; one that is held back ends `not-ready`. */
  async function twoTicks(between: () => void): Promise<string[]> {
    vi.useFakeTimers();
    const outcomes: string[] = [];
    const warn = console.warn;
    console.warn = (): void => {};
    const stop = startAutoBackup({
      accountId: () => new Uint8Array(16),
      knownGroupIds: () => ["ZzEtZ3JvdXAtaWQ="],
      accountIdB64: () => ACCOUNT,
      ed25519Seed: () => new Uint8Array(32),
      deviceKey: async () => ({}) as unknown as DeviceCipher,
      sessionToken: () => "token",
      tickMs: 1000,
      onOutcome: (o) => outcomes.push(o),
    });
    try {
      await vi.advanceTimersByTimeAsync(10);
      between();
      await vi.advanceTimersByTimeAsync(1000);
    } finally {
      stop();
      console.warn = warn;
    }
    return outcomes;
  }

  it("reads the owed flag from its store: held back while owed, attempted once cleared", async () => {
    markBackupRestoreOwed(ACCOUNT);
    const outcomes = await twoTicks(() => clearBackupRestoreOwed(ACCOUNT));
    expect(outcomes).toEqual(["not-ready", "failed"]);
  });

  it("the group list it is given marks the first run dirty, owed or not", async () => {
    markBackupRestoreOwed(ACCOUNT);
    await twoTicks(() => {});
    const state = loadAutoBackupState(ACCOUNT);
    expect(state.lastGroupsKey).toBe("ZzEtZ3JvdXAtaWQ=");
    // Still dirty: the held-back upload goes out once the restore ran.
    expect(state.dirtySinceMs).not.toBeNull();
    expect(state.lastUploadAtMs).toBe(0);
  });
});

describe("the own profile and setting", () => {
  const ACCOUNT = "own-state";
  beforeEach(() => {
    (globalThis as unknown as Record<string, unknown>)["localStorage"] = new MemoryStorage();
  });
  afterEach(() => {
    delete (globalThis as unknown as Record<string, unknown>)["localStorage"];
  });

  it("the first report counts as a change, an unchanged key does not, a moved stamp does", () => {
    expect(ownStateChanged(ACCOUNT, "5:7")).toBe(true);
    expect(ownStateChanged(ACCOUNT, "5:7")).toBe(false);
    expect(ownStateChanged(ACCOUNT, "6:7")).toBe(true);
    expect(ownStateChanged(ACCOUNT, "6:8")).toBe(true);
  });
});
