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
import { type DeviceCipher } from "../auth/device-cipher";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const putDeviceGroupSnapshot = vi.fn(async () => undefined);
vi.mock("../devices/device-group-snapshot-store", () => ({
  getDeviceGroupSnapshot: async () => null,
  getDeviceGroupSnapshotState: async () => null,
  deleteDeviceGroupSnapshot: async () => undefined,
  putDeviceGroupSnapshot: (...a: unknown[]) => putDeviceGroupSnapshot(...(a as [])),
}));

/** ADR-0081 decision 4: these foundings are a single-device account's. */
const ALONE = { mayFound: async (): Promise<boolean> => true };

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { ContactsClient } from "../contacts/client";
import type { DiscoveryClient } from "../discovery/client";
import type { DsClient } from "./client";
import { dsAccess } from "./access";
import { ensureDeviceGroup } from "../devices/lifecycle";
import {
  foundPublisherGroup,
  openHeldPublisherGroup,
  type UnconfirmedFoundingStorage,
} from "../channels/composer";
import { installPublisherDeviceSource } from "../channels/publisher-device";
import { AccountStoresSealedError } from "../auth/account-store-gate";
import { durableSnapshotHook } from "../chat/persist-hooks";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

afterEach(() => {
  vi.unstubAllGlobals();
  dsAccess.setAuthorityFactory(null);
  putDeviceGroupSnapshot.mockClear();
});

/** The peer's `reply` grant (ADR-0060), as accepting their contact request
 *  holds it: its mailbox binding (ADR-0078) is the peer's, so the 1:1
 *  Welcome has a verified mailbox to go to. */
function replyGrantOf(peer: wasm.Identity): string {
  const grant = wasm.deriveReachGrant(peer.ed25519Seed, wasm.ReachClass.Reply, new Uint8Array(16).fill(7), 0);
  try {
    return Buffer.from(wasm.encodeReachGrant(grant)).toString("base64");
  } finally {
    grant.free();
  }
}

function httpError(status: number): Error {
  return Object.assign(new Error(`HTTP ${status}`), { status });
}

describe("a conversation group: no Welcome before the claim lands", () => {
  function setup(claimStatus: number, onPersist?: (order: string[]) => Promise<void>) {
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
      ...(onPersist === undefined ? {} : { onPersist: () => onPersist(order) }),
    });
    // The key the caller expects the peer to have: the one its KeyPackage's
    // binding names. `startGroupWith` adds only a device bound to it
    // (ADR-0073 R.2, `contacts/add-member-key.ts`).
    const peerKey = peer.signaturePublicKey;
    const replyGrantB64 = replyGrantOf(peer);
    return {
      client,
      ds,
      order,
      peerKey,
      replyGrantB64,
      free: () => [device, account, peer].forEach((i) => i.free()),
    };
  }

  it("204: the claim goes first, then the Welcome", async () => {
    const s = setup(204);
    try {
      await s.client.startGroupWith(null, new Uint8Array(16).fill(1), s.peerKey, "chat", {
          replyGrantB64: s.replyGrantB64,
        });
      expect(s.order).toEqual(["claim:claim", "welcome"]);
      // ADR-0078: the Welcome is a row SEALED to the peer's mailbox key (the
      // one the reply grant binds), addressed to that key's address.
      const call = (s.ds.publishMailbox as unknown as { mock: { calls: [Uint8Array, Uint8Array][] } }).mock
        .calls[0]!;
      const grant = wasm.decodeReachGrant(Buffer.from(s.replyGrantB64, "base64"));
      const pk = grant.mailboxPkVerified(s.peerKey);
      grant.free();
      expect(Array.from(call[0])).toEqual(Array.from(wasm.mailboxAddress(pk)));
      expect(wasm.isWellFormedMailboxRow(call[1])).toBe(true);
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
          s.client.startGroupWith(null, new Uint8Array(16).fill(1), s.peerKey, "chat", {
          replyGrantB64: s.replyGrantB64,
        }),
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

describe("a conversation group: no Welcome before the founder's state is on disk", () => {
  function setup(onPersist: (order: string[]) => Promise<void>) {
    const device = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x24));
    const account = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x25));
    const peer = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x26));
    const order: string[] = [];
    const ds = {
      putStatement: vi.fn(async () => {
        order.push("claim");
      }),
      publishMailbox: vi.fn(async () => {
        order.push("welcome");
        return { id: 1, createdAt: "t" };
      }),
    } as unknown as DsClient;
    const discovery = {
      identityFetch: async () => null,
      fetchKeyPackages: async () => [{ deviceId: peer.signaturePublicKey, keyPackage: peer.createKeyPackage() }],
    } as unknown as DiscoveryClient;
    const client = new ContactsClient(discovery, ds, {
      identityProvider: () => device,
      accountIdentityProvider: () => account,
      onPersist: () => onPersist(order),
    });
    return {
      client,
      ds,
      order,
      peerKey: peer.signaturePublicKey,
      replyGrantB64: replyGrantOf(peer),
      free: () => [device, account, peer].forEach((i) => i.free()),
    };
  }

  it("the founder's snapshot is written after the claim and before the Welcome", async () => {
    const s = setup(async (order) => {
      order.push("persist");
    });
    try {
      await s.client.startGroupWith(null, new Uint8Array(16).fill(1), s.peerKey, "chat", {
          replyGrantB64: s.replyGrantB64,
        });
      // The second persist is the best-effort one after the delivery.
      expect(s.order).toEqual(["claim", "persist", "welcome", "persist"]);
    } finally {
      s.free();
    }
  });

  it("a refused persist sends no Welcome (a sealed account, an ended session, a disk failure)", async () => {
    const s = setup(async (order) => {
      order.push("persist");
      throw new AccountStoresSealedError();
    });
    try {
      await expect(
        s.client.startGroupWith(null, new Uint8Array(16).fill(1), s.peerKey, "chat", {
          replyGrantB64: s.replyGrantB64,
        }),
      ).rejects.toBeDefined();
      expect(s.order).toEqual(["claim", "persist"]);
      expect(s.ds.publishMailbox).not.toHaveBeenCalled();
    } finally {
      s.free();
    }
  });

  it("the durable hook refuses when no account is signed in, so nothing is sent", async () => {
    const persistSnapshotDurable = vi.fn(async () => undefined);
    const hook = durableSnapshotHook({ persistSnapshotDurable }, () => null);
    const s = setup(async (order) => {
      order.push("persist");
      await hook();
    });
    try {
      await expect(
        s.client.startGroupWith(null, new Uint8Array(16).fill(1), s.peerKey, "chat", {
          replyGrantB64: s.replyGrantB64,
        }),
      ).rejects.toBeDefined();
      expect(persistSnapshotDurable).not.toHaveBeenCalled();
      expect(s.ds.publishMailbox).not.toHaveBeenCalled();
    } finally {
      s.free();
    }
  });
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
        {} as unknown as DeviceCipher,
        new Uint8Array(64).fill(0x32),
        ALONE,
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
      ensureDeviceGroup(ACCOUNT_ID, "acc", new Uint8Array(64).fill(0x33), {} as unknown as DeviceCipher, undefined, ALONE),
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
      {} as unknown as DeviceCipher,
      new Uint8Array(64).fill(0x35),
      ALONE,
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

  /** ADR-0090: the group's leaf is this DEVICE, bound to the owner's account. */
  function ownerWithDevice(fill: number): { owner: wasm.Identity; uninstall: () => void } {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(fill));
    const device = wasm.Identity.fromSeedBound(new Uint8Array(64).fill(fill + 0x80), owner.ed25519Seed, 1_700_000_000n);
    const uninstall = installPublisherDeviceSource(() => device);
    return { owner, uninstall };
  }

  it("a failed claim throws; the unconfirmed founding makes the next open claim again", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const { owner, uninstall } = ownerWithDevice(0x41);
    const channelId = new Uint8Array(16).fill(0x42);
    const st = mem();
    try {
      const failing = vi.fn(async () => new Response("{}", { status: 500 }));
      vi.stubGlobal("fetch", failing);
      await expect(foundPublisherGroup(wasm, owner, channelId, st, ACCOUNT)).rejects.toBeDefined();
      expect(st.data.size).toBe(1);

      // The group now exists locally; the next open still claims.
      const ok = vi.fn(async () => new Response(null, { status: 204 }));
      vi.stubGlobal("fetch", ok);
      const handle = await openHeldPublisherGroup(wasm, owner, channelId, st, ACCOUNT);
      expect(ok).toHaveBeenCalledTimes(1);
      expect(st.data.size).toBe(0);
      handle.free();

      // Claimed and confirmed: a later open claims nothing.
      const later = vi.fn(async () => new Response(null, { status: 204 }));
      vi.stubGlobal("fetch", later);
      (await openHeldPublisherGroup(wasm, owner, channelId, st, ACCOUNT)).free();
      expect(later).not.toHaveBeenCalled();
    } finally {
      uninstall();
      owner.free();
      warn.mockRestore();
    }
  });

  it("the marker is the account's: another account on the browser neither reads nor clears it", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const { owner, uninstall } = ownerWithDevice(0x51);
    const channelId = new Uint8Array(16).fill(0x52);
    const st = mem();
    try {
      vi.stubGlobal("fetch", vi.fn(async () => new Response("{}", { status: 500 })));
      await expect(foundPublisherGroup(wasm, owner, channelId, st, ACCOUNT)).rejects.toBeDefined();
      expect([...st.data.keys()]).toEqual([
        expect.stringContaining(`lakko/publisher-group-unclaimed/v2/${ACCOUNT}/`),
      ]);

      // Another account's open does not see the first account's marker.
      const other = vi.fn(async () => new Response(null, { status: 204 }));
      vi.stubGlobal("fetch", other);
      (await openHeldPublisherGroup(wasm, owner, channelId, st, OTHER_ACCOUNT)).free();
      expect(other).not.toHaveBeenCalled();
      expect(st.data.size).toBe(1);

      // The first account still owes its claim.
      const again = vi.fn(async () => new Response(null, { status: 204 }));
      vi.stubGlobal("fetch", again);
      (await openHeldPublisherGroup(wasm, owner, channelId, st, ACCOUNT)).free();
      expect(again).toHaveBeenCalledTimes(1);
      expect(st.data.size).toBe(0);
    } finally {
      uninstall();
      owner.free();
      warn.mockRestore();
    }
  });

  it("with no account nothing is written and every open claims again", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const { owner, uninstall } = ownerWithDevice(0x61);
    const channelId = new Uint8Array(16).fill(0x62);
    const st = mem();
    try {
      const ok = vi.fn(async () => new Response(null, { status: 204 }));
      vi.stubGlobal("fetch", ok);
      (await foundPublisherGroup(wasm, owner, channelId, st, null)).free();
      (await openHeldPublisherGroup(wasm, owner, channelId, st, null)).free();
      expect(ok).toHaveBeenCalledTimes(2);
      expect(st.data.size).toBe(0);
    } finally {
      uninstall();
      owner.free();
      warn.mockRestore();
    }
  });

  it("ADR-0090: founding refuses where this device already holds the group (delete first)", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const { owner, uninstall } = ownerWithDevice(0x63);
    const channelId = new Uint8Array(16).fill(0x64);
    const st = mem();
    try {
      vi.stubGlobal("fetch", vi.fn(async () => new Response(null, { status: 204 })));
      (await foundPublisherGroup(wasm, owner, channelId, st, ACCOUNT)).free();
      await expect(foundPublisherGroup(wasm, owner, channelId, st, ACCOUNT)).rejects.toBeDefined();
    } finally {
      uninstall();
      owner.free();
      warn.mockRestore();
    }
  });
});
