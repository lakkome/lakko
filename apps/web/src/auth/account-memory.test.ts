// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The in-memory half of Forget device (`account-memory.ts`): a cache that
 * holds an account's rows and saves them later is dropped when the wipe seals
 * the account and again when that account lands on the page anew, so the
 * reopened store gate cannot be fed pre-wipe data from memory.
 */
import { afterEach, beforeEach, describe, expect, it, onTestFinished, vi } from "vitest";

import { MemoryStorage } from "../test/fake-storage";
import { base64Encode } from "./account-id";
import {
  dropAccountMemory,
  registerAccountMemory,
  registeredAccountMemory,
} from "./account-memory";
import {
  WIPED_MARKER_KEY,
  finishAccountWipe,
  installAccountStorageGate,
  openAccountStores,
  resetAccountStoreGateForTests,
  sealAccountStoresForWipe,
  accountStoresWritable,
} from "./account-store-gate";
import { accountStoreIds } from "./wipe-account";
import {
  applyGroupCallEventFor,
  groupCallsFor,
} from "../calls/group-call-store";
import { heldSends, resetHeldSends } from "../chat/held-send-queue";
import { channelsActions, channelsState } from "../state/channels-state";
import {
  dropPendingAddress,
  installAddressRegister,
  rememberAddress,
} from "./protect/address-register";
import {
  dropPendingUsername,
  installUsernameRegister,
  rememberUsername,
  usernameWritePending,
} from "./username-register";
import { ownDeviceRoster, publishOwnDeviceRoster } from "../devices/own-device-roster";
import { bindAddedNotice, addedNotice, recordRoster } from "../devices/added-notice";
import { armRetry, armedRetries, retryScopeKey } from "../chat/membership-retry";

const A = base64Encode(new Uint8Array(16).fill(0xfb));
const B = base64Encode(new Uint8Array(16).fill(0x5a));

let local: MemoryStorage;

beforeEach(() => {
  local = new MemoryStorage();
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = local;
  g["sessionStorage"] = new MemoryStorage();
  resetAccountStoreGateForTests();
});

afterEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["sessionStorage"];
  resetAccountStoreGateForTests();
  resetHeldSends();
  dropPendingAddress();
  dropPendingUsername();
  publishOwnDeviceRoster(null);
  bindAddedNotice(null);
});

const settle = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

describe("the registry", () => {
  it("hands every form of the account to every drop; a sign-out runs only the sign-out drops", () => {
    const wipeOnly = vi.fn();
    const both = vi.fn();
    const offA = registerAccountMemory("test-wipe-only", wipeOnly);
    const offB = registerAccountMemory("test-both", both, { signOut: true });
    try {
      dropAccountMemory(["a", "a_url"], "wipe");
      expect(wipeOnly).toHaveBeenCalledWith(["a", "a_url"]);
      expect(both).toHaveBeenCalledWith(["a", "a_url"]);
      wipeOnly.mockClear();
      both.mockClear();
      dropAccountMemory(["a"], "sign-out");
      expect(wipeOnly).not.toHaveBeenCalled();
      expect(both).toHaveBeenCalledWith(["a"]);
    } finally {
      offA();
      offB();
    }
  });

  it("a drop that throws is named and the others still run", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const after = vi.fn();
    const offA = registerAccountMemory("test-throws", () => {
      throw new Error("boom");
    });
    const offB = registerAccountMemory("test-after", after);
    try {
      expect(dropAccountMemory(["a"])).toEqual(["test-throws"]);
      expect(after).toHaveBeenCalled();
    } finally {
      offA();
      offB();
      warn.mockRestore();
    }
  });

  it("lists the caches this suite reaches (a removed registration fails here)", () => {
    expect(registeredAccountMemory()).toEqual(
      expect.arrayContaining([
        "channels-state",
        "device-added-notice",
        "group-calls",
        "held-sends",
        "membership-retries",
        "own-device-roster",
        "recovery-address-register",
        "username-register",
      ]),
    );
  });
});

describe("the gate drops memory", () => {
  it("when a wipe seals the account, before any delete", () => {
    const drop = vi.fn();
    const off = registerAccountMemory("test-seal", drop);
    try {
      sealAccountStoresForWipe(accountStoreIds(A));
      expect(drop).toHaveBeenCalledWith(accountStoreIds(A));
    } finally {
      off();
    }
  });

  it("when an account sealed by a wipe lands again, and not at an ordinary landing", () => {
    const drop = vi.fn();
    const off = registerAccountMemory("test-reopen", drop);
    try {
      openAccountStores(accountStoreIds(B));
      expect(drop).not.toHaveBeenCalled();
      const ids = accountStoreIds(A);
      sealAccountStoresForWipe(ids);
      finishAccountWipe(ids);
      drop.mockClear();
      openAccountStores(ids);
      expect(drop).toHaveBeenCalledTimes(1);
      expect(accountStoresWritable(A)).toBe(true);
      // The next landing of the open account drops nothing.
      openAccountStores(ids);
      expect(drop).toHaveBeenCalledTimes(1);
    } finally {
      off();
    }
  });

  it("a landing during the wipe drops at the next landing, which opens the account", () => {
    const drop = vi.fn();
    const off = registerAccountMemory("test-overlap", drop);
    try {
      const ids = accountStoreIds(A);
      sealAccountStoresForWipe(ids);
      drop.mockClear();
      openAccountStores(ids);
      expect(drop).not.toHaveBeenCalled();
      finishAccountWipe(ids);
      openAccountStores(ids);
      expect(drop).toHaveBeenCalledTimes(1);
    } finally {
      off();
    }
  });

  it("in another tab, when the account lands there after a wipe it has not absorbed yet", () => {
    const drop = vi.fn();
    const off = registerAccountMemory("test-other-tab", drop);
    try {
      sealAccountStoresForWipe(accountStoreIds(A));
      expect(local.getItem(WIPED_MARKER_KEY)).not.toBeNull();
      // Another tab: its own memory, the same localStorage.
      resetAccountStoreGateForTests();
      drop.mockClear();
      openAccountStores(accountStoreIds(A));
      expect(drop).toHaveBeenCalledTimes(1);
    } finally {
      off();
    }
  });
});

describe("the caches", () => {
  it("group-call records refilled after the wipe step are gone at the next landing", () => {
    // The real Storage gate in front of the tab storage: the sealed account's
    // write is dropped there, so only memory still holds the late record.
    const g = globalThis as unknown as Record<string, unknown>;
    g["Storage"] = MemoryStorage;
    const tab = new MemoryStorage();
    g["sessionStorage"] = tab;
    const uninstall = installAccountStorageGate();
    onTestFinished(() => {
      uninstall();
      delete g["Storage"];
    });
    const G = "R0lE";
    const ids = accountStoreIds(A);
    sealAccountStoresForWipe(ids);
    finishAccountWipe(ids);
    // A receive that was in flight across the wipe folds a call in.
    applyGroupCallEventFor(
      A,
      G,
      {
        kind: "start",
        callIdHex: "c1".repeat(16),
        initiatorLeaf: 1,
        initiatorPersonKey: null,
        createdAtMs: 1_000,
        mediaAudio: true,
        mediaVideo: false,
        observedAtMs: 1_000,
      },
      1_000,
    );
    expect(groupCallsFor(A, G).size).toBe(1);
    expect(tab.entries()).toEqual([]);
    openAccountStores(ids);
    expect(groupCallsFor(A, G).size).toBe(0);
  });

  it("messages held for the wiped session go with the seal", () => {
    heldSends.hold("c0ffee", { text: "late", attached: [], reply: undefined }, 1);
    sealAccountStoresForWipe(accountStoreIds(A));
    expect(heldSends.all()).toEqual([]);
  });

  it("a channel put in memory after the wipe step is not merged into the next hydration", () => {
    const ids = accountStoreIds(A);
    sealAccountStoresForWipe(ids);
    finishAccountWipe(ids);
    channelsActions.addChannel({
      id: "00".repeat(16),
      localName: "late",
      localDescription: "",
      addedAtMs: 1,
    });
    expect(channelsState.channels()).toHaveLength(1);
    openAccountStores(ids);
    expect(channelsState.channels()).toEqual([]);
  });

  it("a waiting recovery-address write of the wiped account is never made", async () => {
    rememberAddress(A, "name@example.com");
    rememberAddress(B, "other@example.com");
    sealAccountStoresForWipe(accountStoreIds(A));
    const writes: (string | null)[] = [];
    const uninstall = installAddressRegister({
      accountIdB64: A,
      set: async (address) => {
        writes.push(address);
      },
    });
    try {
      await settle();
      expect(writes).toEqual([]);
    } finally {
      uninstall();
    }
  });

  it("a waiting username write of the wiped account is never made; a sign-out keeps it", async () => {
    rememberUsername(A, "alice");
    dropAccountMemory(accountStoreIds(A), "sign-out");
    expect(usernameWritePending(A)).toBe(true);
    sealAccountStoresForWipe(accountStoreIds(A));
    expect(usernameWritePending(A)).toBe(false);
    const writes: string[] = [];
    const uninstall = installUsernameRegister({
      accountIdB64: A,
      set: async (username) => {
        writes.push(username);
      },
    });
    try {
      await settle();
      expect(writes).toEqual([]);
    } finally {
      uninstall();
    }
  });

  it("the own-device roster of the wiped account admits nothing afterwards", () => {
    publishOwnDeviceRoster({ accountIdB64: A, keysHex: ["aa"] });
    sealAccountStoresForWipe(accountStoreIds(A));
    expect(ownDeviceRoster(A)).toBeNull();
  });

  it("the device-added notice held for the wiped account is dropped", () => {
    bindAddedNotice(A);
    recordRoster(A, { keys: ["aa", "bb"], ownKey: "aa", addedHere: [] }, 1);
    recordRoster(A, { keys: ["aa", "bb", "cc"], ownKey: "aa", addedHere: [] }, 2);
    expect(addedNotice().newKeys).toEqual(["cc"]);
    sealAccountStoresForWipe(accountStoreIds(A));
    expect(addedNotice().newKeys).toEqual([]);
  });

  it("armed membership retries of the wiped account go; another account's stay", () => {
    const scopeA = retryScopeKey(A, "Zw==");
    const scopeB = retryScopeKey(B, "Zw==");
    const armed = { request: { kind: "leave", subjectKeyB64: "a2V5" } } as never;
    armRetry(scopeA, armed);
    armRetry(scopeB, armed);
    sealAccountStoresForWipe(accountStoreIds(A));
    expect(armedRetries(scopeA)).toEqual([]);
    expect(armedRetries(scopeB)).toHaveLength(1);
  });
});
