// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0081 decisions 3 and 4 in `ensureDeviceGroup` over the REAL core: a
 * device with no usable DeviceGroup state founds only when the account has no
 * other device; otherwise it is told to rejoin and nothing is founded,
 * claimed or reset. An unreadable snapshot is dropped first. Every snapshot
 * write fails closed.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const store = vi.hoisted(() => ({
  state: null as null | "unreadable" | { snapshot: Uint8Array; generation: number },
  deleted: 0,
  put: [] as Uint8Array[],
  putFails: false,
}));
vi.mock("./device-group-snapshot-store", () => ({
  getDeviceGroupSnapshotState: async () => {
    if (store.state === "unreadable") throw new Error("OperationError");
    return store.state;
  },
  deleteDeviceGroupSnapshot: async () => {
    store.deleted += 1;
    store.state = null;
  },
  putDeviceGroupSnapshot: async (_a: string, snapshot: Uint8Array) => {
    if (store.putFails) throw new Error("QuotaExceededError");
    store.put.push(snapshot);
    return store.put.length;
  },
}));

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { DeviceGroupNotJoinedError, ensureDeviceGroup, persistDeviceGroupSnapshot } from "./lifecycle";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

afterEach(() => {
  vi.unstubAllGlobals();
  store.state = null;
  store.deleted = 0;
  store.put = [];
  store.putFails = false;
});

const ACCOUNT_ID = new Uint8Array(16).fill(7);
const DEVICE_SEED = new Uint8Array(32).fill(0x41);
const ACCOUNT_SEED = new Uint8Array(64).fill(0x42);

function stubClaim(): { calls: string[] } {
  const calls: string[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: RequestInfo | URL) => {
      calls.push(String(url).split("/").pop() ?? "");
      return new Response(null, { status: 204 });
    }),
  );
  return { calls };
}

describe("ADR-0081 decision 4 - re-founding is a single-device account's", () => {
  it("another own device listed: not founded, not claimed - the device rejoins", async () => {
    const claim = stubClaim();
    const err = await ensureDeviceGroup(ACCOUNT_ID, "acc", DEVICE_SEED, {} as CryptoKey, ACCOUNT_SEED, {
      mayFound: async () => false,
    }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(DeviceGroupNotJoinedError);
    expect((err as DeviceGroupNotJoinedError).reason).toBe("rejoin");
    expect(claim.calls).toEqual([]);
    expect(store.put).toEqual([]);
  });

  it("no founding policy (a one-shot path): never founds", async () => {
    const claim = stubClaim();
    const err = await ensureDeviceGroup(ACCOUNT_ID, "acc", DEVICE_SEED, {} as CryptoKey, ACCOUNT_SEED).catch(
      (e: unknown) => e,
    );
    expect((err as DeviceGroupNotJoinedError).reason).toBe("no-state");
    expect(claim.calls).toEqual([]);
  });

  it("a directory that cannot be read founds nothing", async () => {
    const claim = stubClaim();
    await expect(
      ensureDeviceGroup(ACCOUNT_ID, "acc", DEVICE_SEED, {} as CryptoKey, ACCOUNT_SEED, {
        mayFound: async () => {
          throw new Error("discovery unreachable");
        },
      }),
    ).rejects.toThrow(/discovery/);
    expect(claim.calls).toEqual([]);
  });

  it("a single-device account founds: the claim, then the snapshot", async () => {
    const claim = stubClaim();
    const ensured = await ensureDeviceGroup(ACCOUNT_ID, "acc", DEVICE_SEED, {} as CryptoKey, ACCOUNT_SEED, {
      mayFound: async () => true,
    });
    expect(ensured.bootstrapped).toBe(true);
    expect(claim.calls).toEqual(["claim"]);
    expect(store.put).toHaveLength(1);
    ensured.group.free();
    ensured.identity.free();
  });

  it("an unreadable snapshot is dropped before the decision (decision 4, step 1)", async () => {
    stubClaim();
    store.state = "unreadable";
    const err = await ensureDeviceGroup(ACCOUNT_ID, "acc", DEVICE_SEED, {} as CryptoKey, ACCOUNT_SEED, {
      mayFound: async () => false,
    }).catch((e: unknown) => e);
    expect(store.deleted).toBe(1);
    expect((err as DeviceGroupNotJoinedError).reason).toBe("rejoin");
  });
});

describe("ADR-0081 decision 3 - a DeviceGroup snapshot write fails closed", () => {
  it("the founding's write failing fails the founding", async () => {
    stubClaim();
    store.putFails = true;
    await expect(
      ensureDeviceGroup(ACCOUNT_ID, "acc", DEVICE_SEED, {} as CryptoKey, ACCOUNT_SEED, { mayFound: async () => true }),
    ).rejects.toThrow(/Quota/);
  });

  it("persistDeviceGroupSnapshot rejects instead of swallowing", async () => {
    store.putFails = true;
    const identity = wasm.Identity.fromSeed(DEVICE_SEED);
    try {
      await expect(persistDeviceGroupSnapshot(identity, "acc", {} as CryptoKey)).rejects.toThrow(/Quota/);
    } finally {
      identity.free();
    }
  });
});
