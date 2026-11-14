// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX `second-device.md` section 4a: a device of a channel's owner or a
 * publisher that holds no PublisherGroup for it is a reader there. Decided by
 * the account's register (installed through its port, as the DeviceGroup
 * provider does) and group presence (publisher keys are set only once the
 * group loaded or was founded in New channel).
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { channelReaderDeviceRoleOf, channelRoleOf, channelRotatesHere } from "./channel-access";
import {
  installChannelSubscriptionRegister,
  type ChannelSubscriptionPort,
  type ChannelSubscriptionView,
} from "./channel-subscription-register";
import { readerDeviceSections } from "./channel-subscription-plan";
import { channelsActions } from "../state/channels-state";

const OWNED = "41".repeat(16);
const CO = "42".repeat(16);
const PLAIN = "43".repeat(16);
const KEYS = { seed: new Uint8Array(32).fill(1), signingKeySeed: new Uint8Array(32).fill(2), seedEpoch: 0 };

function register(idHex: string, role: ChannelSubscriptionView["role"]): ChannelSubscriptionView {
  return {
    channelIdHex: idHex,
    hlc: 1n,
    subscribed: true,
    role,
    subscriberSecret: new Uint8Array(32).fill(3),
    readKey: new Uint8Array(0),
    publisherGroupIdHex: role === "co-publisher" ? "ab".repeat(32) : "",
    ownerAccountKey: new Uint8Array(0),
    name: "",
    handle: "",
  };
}

let uninstall: (() => void) | null = null;

beforeEach(() => {
  const held = new Map([
    [OWNED, register(OWNED, "owner")],
    [CO, register(CO, "co-publisher")],
    [PLAIN, register(PLAIN, "subscriber")],
  ]);
  const port: ChannelSubscriptionPort = {
    accountIdB64: "YWNjb3VudA==",
    all: () => [...held.values()],
    get: (id) => held.get(id),
    set: async () => false,
    leave: async () => false,
  };
  uninstall = installChannelSubscriptionRegister(port);
  for (const id of [OWNED, CO, PLAIN]) {
    channelsActions.addChannel({ id, localName: "C", localDescription: "", addedAtMs: 1 });
    channelsActions.setSubscriberKeys(id, { secret: new Uint8Array(32).fill(3), public: new Uint8Array(32).fill(4) });
  }
});

afterEach(() => {
  uninstall?.();
  for (const id of [OWNED, CO, PLAIN]) channelsActions.removeChannel(id);
});

describe("a reader device of the owner or a publisher", () => {
  it("without the PublisherGroup: a reader, shown a subscriber's channel", () => {
    expect(channelReaderDeviceRoleOf(OWNED)).toBe("owner");
    expect(channelReaderDeviceRoleOf(CO)).toBe("publisher");
    expect(channelReaderDeviceRoleOf(PLAIN)).toBeNull();
    // No composer, no publisher controls: the subscriber role.
    expect(channelRoleOf(OWNED)).toBe("subscriber");
    expect(channelRoleOf(CO)).toBe("subscriber");
    // Nothing rotates here.
    expect(channelRotatesHere(OWNED)).toBe(false);
    // The exit block is empty.
    expect(readerDeviceSections(["subscribers", "members-can-find-me", "unsubscribe"], channelReaderDeviceRoleOf(OWNED))).toEqual([
      "subscribers",
      "members-can-find-me",
    ]);
  });

  it("with the PublisherGroup: the publishing device, not a reader", () => {
    channelsActions.setKeys(OWNED, KEYS);
    expect(channelReaderDeviceRoleOf(OWNED)).toBeNull();
    expect(channelRoleOf(OWNED)).toBe("owner");
    expect(channelRotatesHere(OWNED)).toBe(true);
  });

  it("keys without a co-publisher mark do not make an owner when the register says co-publisher", () => {
    channelsActions.setKeys(CO, KEYS);
    expect(channelRotatesHere(CO)).toBe(false);
    expect(channelRoleOf(CO)).toBe("publisher");
  });
});
