// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The channel-subscription register between two devices of one account,
 * through the REAL `SyncedState` fold and exactly the helpers the DeviceGroup
 * provider runs: written on one device, its envelope applied on the other,
 * planned there into a reader's channel; the leave follows.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  allChannelSubscriptionViews,
  channelSubscriptionView,
  writeChannelLeave,
  writeChannelSubscription,
} from "./channel-subscription-wasm";
import { planChannelSubscriptions } from "./channel-subscription-plan";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const ID = "c1".repeat(16);
const GROUP = "ab".repeat(32);

function secret(): Uint8Array {
  const kp = wasm.ChannelSubscriberKeypair.generate();
  try {
    return kp.secret();
  } finally {
    kp.free();
  }
}

describe("the channel-subscription register reaches another device of the account", () => {
  it("a subscription, then the leave", () => {
    const first = new wasm.SyncedState();
    const second = new wasm.SyncedState();
    try {
      const key = secret();
      const envelope = writeChannelSubscription(first, wasm, 1n, ID, {
        role: "subscriber",
        subscriberSecret: key,
        readKey: new Uint8Array(32).fill(5),
        publisherGroupIdHex: "",
        ownerAccountKey: new Uint8Array(0),
        name: "Daily news",
        handle: "news",
      });
      expect(envelope).not.toBeNull();
      // Identical value again: nothing to send.
      expect(
        writeChannelSubscription(first, wasm, 2n, ID, {
          role: "subscriber",
          subscriberSecret: key,
          readKey: new Uint8Array(32).fill(5),
          publisherGroupIdHex: "",
          ownerAccountKey: new Uint8Array(0),
          name: "Daily news",
          handle: "news",
        }),
      ).toBeNull();

      expect(wasm.isChannelSubscriptionEnvelope(envelope as Uint8Array)).toBe(true);
      expect(second.applyChannelSubscriptionEnvelope(envelope as Uint8Array)).toBe(true);
      const view = channelSubscriptionView(second, wasm, ID);
      expect(view).toMatchObject({ subscribed: true, role: "subscriber", name: "Daily news", handle: "news" });
      expect([...(view?.subscriberSecret ?? [])]).toEqual([...key]);

      // The second device plans the channel into its list.
      const held = new Map(allChannelSubscriptionViews(second, wasm).map((v) => [v.channelIdHex, v]));
      const steps = planChannelSubscriptions({
        local: [],
        held,
        intents: new Map(),
        ending: new Set(),
        freshSecret: () => new Uint8Array(32),
      });
      expect(steps.map((s) => s.kind)).toEqual(["add"]);

      const leave = writeChannelLeave(first, 3n, ID);
      expect(leave).not.toBeNull();
      expect(writeChannelLeave(first, 4n, ID)).toBeNull();
      expect(second.applyChannelSubscriptionEnvelope(leave as Uint8Array)).toBe(true);
      expect(channelSubscriptionView(second, wasm, ID)?.subscribed).toBe(false);

      const afterLeave = planChannelSubscriptions({
        local: [
          {
            idHex: ID,
            listed: true,
            publishing: false,
            coPublisherGroupIdHex: null,
            subscriberSecret: key,
            readKey: null,
            holdsSeeds: true,
            name: "Daily news",
            handle: "news",
          },
        ],
        held: new Map(allChannelSubscriptionViews(second, wasm).map((v) => [v.channelIdHex, v])),
        intents: new Map(),
        ending: new Set(),
        freshSecret: () => new Uint8Array(32),
      });
      expect(afterLeave).toEqual([{ kind: "drop", idHex: ID }]);
    } finally {
      first.free();
      second.free();
    }
  });

  it("a co-publisher's register carries its group id; the role is read back", () => {
    const first = new wasm.SyncedState();
    const second = new wasm.SyncedState();
    try {
      const envelope = writeChannelSubscription(first, wasm, 1n, ID, {
        role: "co-publisher",
        subscriberSecret: secret(),
        readKey: new Uint8Array(0),
        publisherGroupIdHex: GROUP,
        ownerAccountKey: new Uint8Array(32).fill(0x4f),
        name: "",
        handle: "",
      });
      expect(second.applyChannelSubscriptionEnvelope(envelope as Uint8Array)).toBe(true);
      expect(channelSubscriptionView(second, wasm, ID)).toMatchObject({
        role: "co-publisher",
        publisherGroupIdHex: GROUP,
        ownerAccountKey: new Uint8Array(32).fill(0x4f),
      });
      // The growth re-send carries every register.
      expect(first.channelSubscriptionEnvelopes().length).toBeGreaterThan(0);
    } finally {
      first.free();
      second.free();
    }
  });

  it("a field out of bounds is refused", () => {
    const s = new wasm.SyncedState();
    try {
      expect(() =>
        writeChannelSubscription(s, wasm, 1n, ID, {
          role: "subscriber",
          subscriberSecret: new Uint8Array(31),
          readKey: new Uint8Array(0),
          publisherGroupIdHex: "",
          ownerAccountKey: new Uint8Array(0),
          name: "",
          handle: "",
        }),
      ).toThrow(/bounds/);
    } finally {
      s.free();
    }
  });
});
