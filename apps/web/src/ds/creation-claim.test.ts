// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057, the second security pass (finding 3): claims fail closed. A new
 * conversation group, a DeviceGroup founding and a channel's PublisherGroup
 * claim their journal and must get 204 BEFORE any Welcome is sent or the
 * channel is announced; a failed claim aborts with nothing published.
 *
 * The groups are the REAL core; the Delivery Service and the snapshot store
 * are fakes.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const putDeviceGroupSnapshot = vi.fn(async () => undefined);
vi.mock("../devices/device-group-snapshot-store", () => ({
  getDeviceGroupSnapshot: async () => null,
  putDeviceGroupSnapshot: (...a: unknown[]) => putDeviceGroupSnapshot(...(a as [])),
}));

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { ContactsClient } from "../contacts/client";
import type { DiscoveryClient } from "../discovery/client";
import type { DsClient } from "./client";
import { dsAccess } from "./access";
import { ensureDeviceGroup } from "../devices/lifecycle";
import { openOrFoundPublisherGroup, type UnconfirmedFoundingStorage } from "../channels/composer";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

afterEach(() => {
  vi.unstubAllGlobals();
  dsAccess.setAuthorityFactory(null);
  putDeviceGroupSnapshot.mockClear();
});

function httpError(status: number): Error {
  return Object.assign(new Error(`HTTP ${status}`), { status });
}

describe("a conversation group: no Welcome before the claim lands", () => {
  function setup(claimStatus: number) {
    const device = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x21));
    const account = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x22));
    const peer = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x23));
    const order: string[] = [];
    const ds = {
      putStatement: vi.fn(async (path: string) => {
        order.push(`claim:${path.split("/").pop() ?? ""}`);
        if (claimStatus >= 400) throw httpError(claimStatus);
      }),
      publishMailbox: vi.fn(async () => {
        order.push("welcome");
        return { id: 1, createdAt: "t" };
      }),
    } as unknown as DsClient;
    const discovery = {
      // ADR-0073 phase 2 — the identity check that runs first; an account
      // that publishes nothing passes (the KeyPackage fetch answers for it).
      identityFetch: async () => null,
      fetchKeyPackages: async () => [{ deviceId: peer.signaturePublicKey, keyPackage: peer.createKeyPackage() }],
    } as unknown as DiscoveryClient;
    const client = new ContactsClient(discovery, ds, {
      identityProvider: () => device,
      accountIdentityProvider: () => account,
    });
    // The key the caller expects the peer to have: the one its KeyPackage's
    // binding names. `startGroupWith` adds only a device bound to it
    // (ADR-0073 R.2, `contacts/add-member-key.ts`).
    const peerKey = peer.signaturePublicKey;
    return { client, ds, order, peerKey, free: () => [device, account, peer].forEach((i) => i.free()) };
  }

  it("204: the claim goes first, then the Welcome", async () => {
    const s = setup(204);
    try {
      await s.client.startGroupWith(null, new Uint8Array(16).fill(1), s.peerKey, "chat");
      expect(s.order).toEqual(["claim:claim", "welcome"]);
    } finally {
      s.free();
    }
  });

  for (const status of [409, 500]) {
    it(`${status}: the creation aborts and no Welcome is sent`, async () => {
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      const s = setup(status);
      try {
        await expect(
          s.client.startGroupWith(null, new Uint8Array(16).fill(1), s.peerKey, "chat"),
        ).rejects.toBeDefined();
        expect(s.order).toEqual(["claim:claim"]);
        expect(s.ds.publishMailbox).not.toHaveBeenCalled();
      } finally {
        s.free();
        warn.mockRestore();
      }
    });
  }
});

describe("a DeviceGroup founding: nothing persisted before the claim lands", () => {
  const ACCOUNT_ID = new Uint8Array(16).fill(7);

  it("a failed claim throws and persists nothing (the next call founds and claims again)", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.stubGlobal("fetch", vi.fn(async () => new Response("{}", { status: 500 })));
    await expect(
      ensureDeviceGroup(
        ACCOUNT_ID,
        "acc",
        new Uint8Array(64).fill(0x31),
        {} as CryptoKey,
        new Uint8Array(64).fill(0x32),
      ),
    ).rejects.toBeDefined();
    expect(putDeviceGroupSnapshot).not.toHaveBeenCalled();
    warn.mockRestore();
  });

  it("no root available at all: refused, nothing persisted", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const fetchSpy = vi.fn(async () => new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchSpy);
    await expect(
      ensureDeviceGroup(ACCOUNT_ID, "acc", new Uint8Array(64).fill(0x33), {} as CryptoKey),
    ).rejects.toBeDefined();
    expect(fetchSpy).not.toHaveBeenCalled();
    expect(putDeviceGroupSnapshot).not.toHaveBeenCalled();
    warn.mockRestore();
  });

  it("204: the claim lands first, then the founding is persisted", async () => {
    const order: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        order.push("claim");
        return new Response(null, { status: 204 });
      }),
    );
    putDeviceGroupSnapshot.mockImplementationOnce(async () => {
      order.push("persist");
    });
    const ensured = await ensureDeviceGroup(
      ACCOUNT_ID,
      "acc",
      new Uint8Array(64).fill(0x34),
      {} as CryptoKey,
      new Uint8Array(64).fill(0x35),
    );
    expect(order).toEqual(["claim", "persist"]);
    ensured.group.free();
    ensured.identity.free();
  });
});

describe("a channel's PublisherGroup: the claim lands before the handle is returned", () => {
  function mem(): UnconfirmedFoundingStorage & { data: Map<string, string> } {
    const data = new Map<string, string>();
    return {
      data,
      getItem: (k) => data.get(k) ?? null,
      setItem: (k, v) => void data.set(k, v),
      removeItem: (k) => void data.delete(k),
    };
  }

  /** The signed-in account the founding marker is recorded for. */
  const ACCOUNT = "Y2FyYS1hY2NvdW50LTAwMDA=";
  const OTHER_ACCOUNT = "ZXZlLWFjY291bnQtMDAwMDA=";

  it("a failed claim throws; the unconfirmed founding makes the next open claim again", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x41));
    const channelId = new Uint8Array(16).fill(0x42);
    const st = mem();
    try {
      const failing = vi.fn(async () => new Response("{}", { status: 500 }));
      vi.stubGlobal("fetch", failing);
      await expect(openOrFoundPublisherGroup(wasm, owner, channelId, st, ACCOUNT)).rejects.toBeDefined();
      expect(st.data.size).toBe(1);

      // The group now exists locally; the next open still claims.
      const ok = vi.fn(async () => new Response(null, { status: 204 }));
      vi.stubGlobal("fetch", ok);
      const handle = await openOrFoundPublisherGroup(wasm, owner, channelId, st, ACCOUNT);
      expect(ok).toHaveBeenCalledTimes(1);
      expect(st.data.size).toBe(0);
      handle.free();

      // Claimed and confirmed: a later open claims nothing.
      const later = vi.fn(async () => new Response(null, { status: 204 }));
      vi.stubGlobal("fetch", later);
      (await openOrFoundPublisherGroup(wasm, owner, channelId, st, ACCOUNT)).free();
      expect(later).not.toHaveBeenCalled();
    } finally {
      owner.free();
      warn.mockRestore();
    }
  });

  it("the marker is the account's: another account on the browser neither reads nor clears it", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x51));
    const channelId = new Uint8Array(16).fill(0x52);
    const st = mem();
    try {
      vi.stubGlobal("fetch", vi.fn(async () => new Response("{}", { status: 500 })));
      await expect(
        openOrFoundPublisherGroup(wasm, owner, channelId, st, ACCOUNT),
      ).rejects.toBeDefined();
      expect([...st.data.keys()]).toEqual([
        expect.stringContaining(`lakko/publisher-group-unclaimed/v2/${ACCOUNT}/`),
      ]);

      // Another account's open does not see the first account's marker.
      const other = vi.fn(async () => new Response(null, { status: 204 }));
      vi.stubGlobal("fetch", other);
      (await openOrFoundPublisherGroup(wasm, owner, channelId, st, OTHER_ACCOUNT)).free();
      expect(other).not.toHaveBeenCalled();
      expect(st.data.size).toBe(1);

      // The first account still owes its claim.
      const again = vi.fn(async () => new Response(null, { status: 204 }));
      vi.stubGlobal("fetch", again);
      (await openOrFoundPublisherGroup(wasm, owner, channelId, st, ACCOUNT)).free();
      expect(again).toHaveBeenCalledTimes(1);
      expect(st.data.size).toBe(0);
    } finally {
      owner.free();
      warn.mockRestore();
    }
  });

  it("with no account nothing is written and every open claims again", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x61));
    const channelId = new Uint8Array(16).fill(0x62);
    const st = mem();
    try {
      const ok = vi.fn(async () => new Response(null, { status: 204 }));
      vi.stubGlobal("fetch", ok);
      (await openOrFoundPublisherGroup(wasm, owner, channelId, st, null)).free();
      (await openOrFoundPublisherGroup(wasm, owner, channelId, st, null)).free();
      expect(ok).toHaveBeenCalledTimes(2);
      expect(st.data.size).toBe(0);
    } finally {
      owner.free();
      warn.mockRestore();
    }
  });
});
