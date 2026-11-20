// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 section 2.5, security pass: refused reads and a DS epoch are not
 * authenticated. A suspected reset stops this device publishing and
 * rotating (its keys go, the channel reads as a waiting reader) and deletes
 * nothing: the group state stays in the provider and is not taken up again
 * by the reader pass. The store and the registry are doubles; the group is
 * the REAL core.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm, wasmSync: () => wasm }));

const state = vi.hoisted(() => ({
  keys: new Map<string, unknown>(),
  access: new Map<string, Record<string, unknown>>(),
  released: [] as string[],
}));
vi.mock("../state/channels-state", () => ({
  channelsActions: {
    dropPublisherRole: (id: string) => void state.keys.delete(id),
    accessOf: (id: string) => state.access.get(id) ?? { admittedThroughSeq: 0, admissionSeeds: [] },
    updateAccessDurable: async (id: string, f: (r: Record<string, unknown>) => Record<string, unknown>) => {
      state.access.set(id, f(state.access.get(id) ?? { admittedThroughSeq: 0, admissionSeeds: [] }));
    },
    setKeys: (id: string, k: unknown) => void state.keys.set(id, k),
  },
  channelsState: { keysByChannel: () => state.keys },
}));
vi.mock("./publisher-group-registry", () => ({
  releasePublisherGroup: (id: string) => void state.released.push(id),
  sharedPublisherDevice: async () => ({}),
  sharedPublisherGroup: async () => ({}),
  withPublisherGroup: async () => undefined,
}));

import { liftResetSuspicionIfClear, settleReaderDevices, suspectForeignChain } from "./publisher-devices";
import { installPublisherDeviceSource } from "./publisher-device";
import { installChannelSubscriptionRegister } from "./channel-subscription-register";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const hex = (b: Uint8Array): string => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

describe("a suspected reset (security pass)", () => {
  it("stops publishing, keeps the group, and the reader pass does not take it up again", async () => {
    vi.spyOn(console, "info").mockImplementation(() => undefined);
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x31));
    const device = wasm.Identity.fromSeedBound(new Uint8Array(64).fill(0x41), owner.ed25519Seed, 1_700_000_000n);
    const channelId = new Uint8Array(16).fill(0x51);
    const idHex = hex(channelId);
    const uninstallDevice = installPublisherDeviceSource(() => device);
    const uninstallRegister = installChannelSubscriptionRegister({
      accountIdB64: "acct",
      all: () => [
        {
          channelIdHex: idHex,
          hlc: 1n,
          subscribed: true,
          role: "owner",
          subscriberSecret: new Uint8Array(32),
          readKey: new Uint8Array(0),
          publisherGroupIdHex: "",
          ownerAccountKey: new Uint8Array(0),
          name: "",
          handle: "",
        },
      ],
      get: () => undefined,
      set: async () => true,
      leave: async () => true,
    });
    const group = new wasm.PublisherGroup(device, owner.ed25519Seed, channelId);
    group.free();
    state.keys.set(idHex, { seed: new Uint8Array(32) });
    try {
      await suspectForeignChain(idHex);
      // No longer publishing or rotating here.
      expect(state.keys.has(idHex)).toBe(false);
      expect(state.released).toContain(idHex);
      expect(state.access.get(idHex)?.resetSuspected).toBe(true);
      // The group state is still in the provider.
      const kept = wasm.PublisherGroup.load(device, owner.ed25519Seed, channelId);
      expect(kept).toBeDefined();
      kept?.free();
      // And the reader pass leaves it alone (no keys from the kept group).
      expect(await settleReaderDevices(owner, () => false, async () => undefined)).toEqual([]);
      expect(state.keys.has(idHex)).toBe(false);
    } finally {
      uninstallRegister();
      uninstallDevice();
    }
  });
});

describe("a suspected reset that was a false alarm", () => {
  const setUp = (fill: number) => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(fill));
    const device = wasm.Identity.fromSeedBound(new Uint8Array(64).fill(fill + 1), owner.ed25519Seed, 1_700_000_000n);
    const channelId = new Uint8Array(16).fill(fill + 2);
    const idHex = hex(channelId);
    const uninstallDevice = installPublisherDeviceSource(() => device);
    const uninstallRegister = installChannelSubscriptionRegister({
      accountIdB64: "acct",
      all: () => [
        {
          channelIdHex: idHex,
          hlc: 1n,
          subscribed: true,
          role: "owner",
          subscriberSecret: new Uint8Array(32),
          readKey: new Uint8Array(0),
          publisherGroupIdHex: "",
          ownerAccountKey: new Uint8Array(0),
          name: "",
          handle: "",
        },
      ],
      get: () => undefined,
      set: async () => true,
      leave: async () => true,
    });
    const group = new wasm.PublisherGroup(device, owner.ed25519Seed, channelId);
    const groupId = group.groupId();
    const epoch = Number(group.epoch());
    group.free();
    state.keys.set(idHex, { seed: new Uint8Array(32) });
    return {
      owner,
      idHex,
      groupId,
      epoch,
      done: () => {
        uninstallRegister();
        uninstallDevice();
      },
    };
  };

  it("good signed reads at the kept epoch lift it and the device publishes again", async () => {
    vi.spyOn(console, "info").mockImplementation(() => undefined);
    const s = setUp(0x61);
    try {
      await suspectForeignChain(s.idHex);
      expect(state.keys.has(s.idHex)).toBe(false);
      const signedAt: bigint[] = [];
      const lifted = await liftResetSuspicionIfClear(s.owner, s.idHex, async (groupId, signer) => {
        expect(hex(groupId)).toBe(hex(s.groupId));
        signedAt.push(signer.epoch);
        return { latestEpoch: s.epoch };
      });
      expect(lifted).toBe(true);
      // The read was signed with the kept group's current epoch key.
      expect(signedAt).toEqual([BigInt(s.epoch)]);
      expect(state.access.get(s.idHex)?.resetSuspected).toBeUndefined();
      // Publishing keys from the kept group: the pass publishes and rotates here again.
      const keys = state.keys.get(s.idHex) as { seed: Uint8Array; seedEpoch: number } | undefined;
      expect(keys?.seed.length).toBe(32);
      expect(keys?.seedEpoch).toBe(s.epoch);
    } finally {
      s.done();
    }
  });

  it("still-refused reads, or a journal at another epoch, keep it suspected", async () => {
    vi.spyOn(console, "info").mockImplementation(() => undefined);
    const s = setUp(0x71);
    try {
      await suspectForeignChain(s.idHex);
      const refused = await liftResetSuspicionIfClear(s.owner, s.idHex, async () => {
        throw new Error("401");
      });
      expect(refused).toBe(false);
      const elsewhere = await liftResetSuspicionIfClear(s.owner, s.idHex, async () => ({ latestEpoch: s.epoch + 3 }));
      expect(elsewhere).toBe(false);
      const unnamed = await liftResetSuspicionIfClear(s.owner, s.idHex, async () => ({ latestEpoch: null }));
      expect(unnamed).toBe(false);
      expect(state.access.get(s.idHex)?.resetSuspected).toBe(true);
      expect(state.keys.has(s.idHex)).toBe(false);
      expect(await settleReaderDevices(s.owner, () => false, async () => undefined)).toEqual([]);
    } finally {
      s.done();
    }
  });
});
