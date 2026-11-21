// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 section 4 - the re-admitter's decision wiring: who may act per
 * route, the device-still-the-account's check per route, and the core's
 * `reentryAdmissionVerdict` (real wasm) over the bookkeeping this client
 * keeps (the leaf's last write at the current epoch, handled per epoch,
 * accepted within the hour).
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  deliverReentryRequest,
  handleReentryRequest,
  heldReentryArrivals,
  installReentryIntake,
  noteLeafWrote,
  resetReentryAdmitMemory,
  type ReentryAdmitPorts,
  type ReentryArrival,
} from "./reentry-admit";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const GROUP_ID = new Uint8Array([4, 4, 4]);
const GID = btoa(String.fromCharCode(...GROUP_ID));
const LEAF_KEY = new Uint8Array(32).fill(0xaa);
const LEAF_HEX = "aa".repeat(32);
const OWN = new Uint8Array(32).fill(1);
const PEER = new Uint8Array(32).fill(2);
let now = 50_000_000;

function group(opts: { epoch?: bigint; accountKey?: Uint8Array; valid?: boolean } = {}): WasmGroup {
  return {
    groupId: GROUP_ID,
    epoch: opts.epoch ?? 7n,
    reentryRequestTarget: () => {
      if (opts.valid === false) throw new Error("reentryPairInvalid");
      return { accountKey: opts.accountKey ?? PEER, signatureKey: LEAF_KEY, replacedLeaf: 3, free: () => {} };
    },
  } as unknown as WasmGroup;
}

function arrival(route: ReentryArrival["route"], extra: Partial<ReentryArrival> = {}): ReentryArrival {
  return {
    route,
    groupId: GROUP_ID,
    epochLeft: 7n,
    gapFromMs: 1_234n,
    gapKey: new Uint8Array(32).fill(0x61),
    keyPackage: new Uint8Array([9]),
    ...extra,
  };
}

function ports(g: WasmGroup, over: Partial<ReentryAdmitPorts> = {}) {
  const proposed: Uint8Array[] = [];
  const owed: { epochAfter: bigint; gapFromMs: number; leafKey: Uint8Array; gapKey: Uint8Array }[] = [];
  const p: ReentryAdmitPorts = {
    now: () => now,
    identity: () => ({}) as WasmIdentity,
    ownPersonKeyB64: () => btoa(String.fromCharCode(...OWN)),
    group: (id) => (id === GID ? g : undefined),
    ownRole: () => "member",
    kindOf: () => "chat",
    directoryListsDevice: async () => true,
    admissionVerdict: (i) =>
      Number(
        wasm.reentryAdmissionVerdict(
          i.stillTheAccounts,
          i.lastWriteMs === undefined ? undefined : BigInt(i.lastWriteMs),
          i.handledAtThisEpoch,
          i.lastAcceptedMs === undefined ? undefined : BigInt(i.lastAcceptedMs),
          BigInt(i.nowMs),
        ),
      ),
    propose: async (_g, kp) => void proposed.push(kp),
    noteGapOwed: (o) => void owed.push(o),
    ...over,
  };
  return { p, proposed, owed };
}

beforeEach(() => {
  vi.spyOn(console, "info").mockImplementation(() => {});
  vi.spyOn(console, "warn").mockImplementation(() => {});
  now = 50_000_000;
  resetReentryAdmitMemory();
});

afterEach(() => {
  installReentryIntake(null);
  vi.restoreAllMocks();
});

describe("who acts (section 4)", () => {
  it("route 2: the other person of a one-to-one admits, proposes the pair, and owes the gap", async () => {
    const t = ports(group());
    expect(await handleReentryRequest(arrival("mailbox"), t.p)).toBe("admitted");
    expect(t.proposed.length).toBe(1);
    expect(t.owed).toEqual([
      { groupIdB64: GID, leafKey: LEAF_KEY, gapKey: new Uint8Array(32).fill(0x61), epochAfter: 8n, gapFromMs: 1_234, route: "mailbox" },
    ]);
  });

  it("route 2 in a group, or for this device's own account, is not ours", async () => {
    expect(await handleReentryRequest(arrival("mailbox"), ports(group(), { kindOf: () => "group" }).p)).toBe("not-ours-to-act");
    expect(await handleReentryRequest(arrival("mailbox"), ports(group({ accountKey: OWN })).p)).toBe("not-ours-to-act");
  });

  it("route 3: only the owner or an admin", async () => {
    expect(await handleReentryRequest(arrival("knock"), ports(group()).p)).toBe("not-ours-to-act");
    resetReentryAdmitMemory();
    expect(await handleReentryRequest(arrival("knock"), ports(group(), { ownRole: () => "admin" }).p)).toBe("admitted");
  });

  it("route 1: a device of the same account, and only a device the DeviceGroup holds", async () => {
    const g = group({ accountKey: OWN });
    expect(await handleReentryRequest(arrival("own-device"), ports(group()).p)).toBe("not-ours-to-act");
    expect(await handleReentryRequest(arrival("own-device", { deviceGroupKeysHex: new Set(["bb".repeat(32)]) }), ports(g).p)).toBe(
      "refused",
    );
    expect(await handleReentryRequest(arrival("own-device", { deviceGroupKeysHex: new Set([LEAF_HEX]) }), ports(g).p)).toBe(
      "admitted",
    );
  });
});

describe("the admission verdict (real core)", () => {
  it("a device the directory no longer lists is refused", async () => {
    const t = ports(group(), { directoryListsDevice: async () => false });
    expect(await handleReentryRequest(arrival("mailbox"), t.p)).toBe("refused");
    expect(t.proposed).toEqual([]);
  });

  it("a leaf that wrote at the current epoch within the hour is not behind", async () => {
    noteLeafWrote(GID, btoa(String.fromCharCode(...LEAF_KEY)), 7n, now - 60_000);
    expect(await handleReentryRequest(arrival("mailbox"), ports(group()).p)).toBe("refused");
  });

  it("one re-entry per leaf per epoch, and none within the hour", async () => {
    const t = ports(group());
    expect(await handleReentryRequest(arrival("mailbox"), t.p)).toBe("admitted");
    expect(await handleReentryRequest(arrival("mailbox"), t.p)).toBe("refused");
    now += 2 * 60 * 60_000;
    expect(await handleReentryRequest(arrival("mailbox"), t.p)).toBe("refused");
    const next = ports(group({ epoch: 8n }));
    expect(await handleReentryRequest(arrival("mailbox", { epochLeft: 8n }), next.p)).toBe("admitted");
  });

  it("another epoch, an unknown group or a package the core refuses: nothing", async () => {
    expect(await handleReentryRequest(arrival("mailbox", { epochLeft: 6n }), ports(group()).p)).toBe("other-epoch");
    expect(await handleReentryRequest(arrival("mailbox", { groupId: new Uint8Array([1]) }), ports(group()).p)).toBe("no-group");
    expect(await handleReentryRequest(arrival("mailbox"), ports(group({ valid: false })).p)).toBe("not-a-reentry");
  });
});

describe("intake", () => {
  it("holds what arrives before the runtime, and hands it over on install", () => {
    deliverReentryRequest(arrival("knock"));
    expect(heldReentryArrivals().length).toBe(1);
    const got: ReentryArrival[] = [];
    installReentryIntake((a) => void got.push(a));
    expect(got.length).toBe(1);
    expect(heldReentryArrivals().length).toBe(0);
  });
});
