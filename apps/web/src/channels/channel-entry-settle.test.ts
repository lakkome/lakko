// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0064 section 7, amendment of 2026-11-14, on web: who the reader pass
 * asks to enter from the saved copy. An owned channel this device holds no
 * group for is entered before decision A; a co-publisher's channel, a channel
 * whose group this device holds, a channel under a suspected reset and a
 * channel whose entry awaits the DS are not; "none" falls back to decision A,
 * "owed" waits.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const registerRows: { channelIdHex: string; subscribed: boolean; role: string }[] = [];
const heldKeys = new Map<string, { seed: Uint8Array; signingKeySeed: Uint8Array; seedEpoch: number }>();

vi.mock("./channel-subscription-register", async (orig) => ({
  ...(await orig<typeof import("./channel-subscription-register")>()),
  channelSubscriptionPort: () => ({ all: () => registerRows }),
}));
vi.mock("./publisher-device", async (orig) => ({
  ...(await orig<typeof import("./publisher-device")>()),
  publisherDeviceOrNull: () => ({}) as unknown,
}));
vi.mock("./composer", async (orig) => ({
  ...(await orig<typeof import("./composer")>()),
  heldPublisherKeys: async (_l: unknown, idHex: string) => heldKeys.get(idHex) ?? null,
}));
vi.mock("./channel-entry", async (orig) => ({
  ...(await orig<typeof import("./channel-entry")>()),
  abandonUnrecordedEntry: async () => false,
}));
vi.mock("../wasm", () => ({ loadWasm: async () => ({}), wasmSync: () => ({}) }));

import { settleReaderDevices } from "./publisher-devices";
import { channelsActions } from "../state/channels-state";
import type { WasmIdentity } from "./composer";

const OWNED = "11".repeat(16);
const COPUB = "22".repeat(16);
const HELD = "33".repeat(16);
const SUSPECTED = "44".repeat(16);
const lineage = {} as unknown as WasmIdentity;

beforeEach(() => {
  registerRows.length = 0;
  heldKeys.clear();
});

describe("the reader pass and the entry", () => {
  it("enters only an owned channel it holds no group for, before decision A", async () => {
    registerRows.push(
      { channelIdHex: OWNED, subscribed: true, role: "owner" },
      { channelIdHex: COPUB, subscribed: true, role: "co-publisher" },
      { channelIdHex: HELD, subscribed: true, role: "owner" },
      { channelIdHex: SUSPECTED, subscribed: true, role: "owner" },
    );
    heldKeys.set(HELD, { seed: new Uint8Array(32), signingKeySeed: new Uint8Array(32), seedEpoch: 1 });
    await channelsActions.updateAccessDurable(SUSPECTED, (r) => ({ ...r, resetSuspected: true as const })).catch(() => undefined);
    const asked: string[] = [];
    const refounded: string[] = [];
    const became: string[] = [];
    const out = await settleReaderDevices(
      lineage,
      (id) => {
        refounded.push(id);
        return false;
      },
      async (id) => {
        became.push(id);
      },
      async (id) => {
        asked.push(id);
        return "entered";
      },
    );
    expect(asked).toEqual([OWNED]);
    // Entered: decision A is not asked, the channel publishes here now.
    expect(refounded).toEqual([]);
    expect(became).toEqual([OWNED]);
    expect(out).toContain(OWNED);
    expect(out).toContain(HELD);
    expect(out).not.toContain(COPUB);
  });

  it("falls back to decision A with nothing to enter from, and waits on an owed entry", async () => {
    registerRows.push({ channelIdHex: OWNED, subscribed: true, role: "owner" });
    const refounded: string[] = [];
    await settleReaderDevices(
      lineage,
      (id) => {
        refounded.push(id);
        return false;
      },
      async () => undefined,
      async () => "none",
    );
    expect(refounded).toEqual([OWNED]);
    refounded.length = 0;
    await settleReaderDevices(
      lineage,
      (id) => {
        refounded.push(id);
        return false;
      },
      async () => undefined,
      async () => "owed",
    );
    expect(refounded).toEqual([]);
  });
});
