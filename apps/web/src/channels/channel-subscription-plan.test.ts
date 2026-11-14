// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import type { ChannelSubscriptionView } from "./channel-subscription-register";
import {
  planChannelSubscriptions,
  readerDeviceRole,
  readerDeviceSections,
  registerHandle,
  registerName,
  type ChannelIntent,
  type LocalChannelState,
  type PlanInput,
} from "./channel-subscription-plan";

const ID = "c1".repeat(16);
const GROUP = "ab".repeat(32);
const KEY_A = new Uint8Array(32).fill(1);
const KEY_B = new Uint8Array(32).fill(2);
const FRESH = new Uint8Array(32).fill(9);
const READ = new Uint8Array(32).fill(5);

function local(over: Partial<LocalChannelState> = {}): LocalChannelState {
  return {
    idHex: ID,
    listed: true,
    publishing: false,
    coPublisherGroupIdHex: null,
    subscriberSecret: KEY_A,
    readKey: null,
    holdsSeeds: false,
    name: "Daily news",
    handle: "news",
    ...over,
  };
}

function live(over: Partial<ChannelSubscriptionView> = {}): ChannelSubscriptionView {
  return {
    channelIdHex: ID,
    hlc: 10n,
    subscribed: true,
    role: "subscriber",
    subscriberSecret: KEY_A,
    readKey: new Uint8Array(0),
    publisherGroupIdHex: "",
    ownerAccountKey: new Uint8Array(0),
    name: "Daily news",
    handle: "news",
    ...over,
  };
}

function left(): ChannelSubscriptionView {
  return live({
    subscribed: false,
    subscriberSecret: new Uint8Array(0),
    name: "",
    handle: "",
  });
}

function plan(
  localState: readonly LocalChannelState[],
  held: readonly ChannelSubscriptionView[],
  intents: Record<string, ChannelIntent> = {},
  ending: readonly string[] = [],
): ReturnType<typeof planChannelSubscriptions> {
  const input: PlanInput = {
    local: localState,
    held: new Map(held.map((h) => [h.channelIdHex, h])),
    intents: new Map(Object.entries(intents)),
    ending: new Set(ending),
    freshSecret: () => FRESH,
  };
  return planChannelSubscriptions(input);
}

describe("the channel-subscription plan", () => {
  it("a channel subscribed before the registers existed gets one (rule 2)", () => {
    const steps = plan([local()], []);
    expect(steps).toEqual([
      {
        kind: "write",
        idHex: ID,
        value: {
          role: "subscriber",
          subscriberSecret: KEY_A,
          readKey: new Uint8Array(0),
          publisherGroupIdHex: "",
          ownerAccountKey: new Uint8Array(0),
          name: "Daily news",
          handle: "news",
        },
      },
    ]);
  });

  it("a live register the device holds unchanged asks for nothing", () => {
    expect(plan([local()], [live()])).toEqual([]);
  });

  it("a live register for a channel not listed adds it as a reader", () => {
    const reg = live({ role: "owner" });
    expect(plan([], [reg])).toEqual([{ kind: "add", idHex: ID, register: reg }]);
  });

  it("a channel whose end is running here is not added back", () => {
    expect(plan([], [live()], {}, [ID])).toEqual([]);
  });

  it("a leave drops a listed channel, unless this device subscribed again", () => {
    expect(plan([local()], [left()])).toEqual([{ kind: "drop", idHex: ID }]);
    const again = plan([local()], [left()], { [ID]: "subscribed" });
    expect(again[0]).toMatchObject({ kind: "write", value: { role: "subscriber", subscriberSecret: KEY_A } });
    expect(again).toContainEqual({ kind: "intent-done", idHex: ID });
  });

  it("a leave forgets carried seeds of a channel this device does not list", () => {
    expect(plan([local({ listed: false, holdsSeeds: true })], [left()])).toEqual([
      { kind: "forget-seeds", idHex: ID },
    ]);
    // ...but not while this device is subscribing to it.
    expect(plan([local({ listed: false, holdsSeeds: true })], [left()], { [ID]: "subscribed" })).toEqual([]);
  });

  it("one key per account: a different local key adopts the register's", () => {
    expect(plan([local({ subscriberSecret: KEY_B })], [live()])).toEqual([
      { kind: "adopt-key", idHex: ID, secret: KEY_A, oldSecret: KEY_B },
    ]);
    expect(plan([local({ subscriberSecret: null })], [live()])).toEqual([
      { kind: "adopt-key", idHex: ID, secret: KEY_A, oldSecret: null },
    ]);
  });

  it("the owner's publishing device writes role Owner, with a fresh key when it holds none", () => {
    const steps = plan([local({ publishing: true, subscriberSecret: null, readKey: READ })], []);
    expect(steps).toEqual([
      {
        kind: "write",
        idHex: ID,
        value: {
          role: "owner",
          subscriberSecret: FRESH,
          readKey: READ,
          publisherGroupIdHex: "",
          ownerAccountKey: new Uint8Array(0),
          name: "Daily news",
          handle: "news",
        },
      },
    ]);
  });

  it("a publishing device keeps the account's key and overrules a leave", () => {
    const steps = plan([local({ publishing: true, subscriberSecret: null })], [live({ role: "owner" })]);
    expect(steps).toEqual([]);
    const overLeave = plan([local({ publishing: true, subscriberSecret: null })], [left()]);
    expect(overLeave[0]).toMatchObject({ kind: "write", value: { role: "owner", subscriberSecret: FRESH } });
  });

  it("a co-publisher's publishing device writes its group id and the owner key it recorded", () => {
    const owner = new Uint8Array(32).fill(0x4f);
    const steps = plan([local({ publishing: true, coPublisherGroupIdHex: GROUP, coPublisherOwnerKey: owner })], []);
    expect(steps[0]).toMatchObject({
      kind: "write",
      value: { role: "co-publisher", publisherGroupIdHex: GROUP, subscriberSecret: KEY_A, ownerAccountKey: owner },
    });
  });

  it("ADR-0090: a co-publisher device with no recorded owner key writes no register yet", () => {
    expect(plan([local({ publishing: true, coPublisherGroupIdHex: GROUP })], []).filter((s) => s.kind === "write")).toEqual([]);
  });

  it("a reader device follows the owner's register and writes nothing", () => {
    expect(plan([local()], [live({ role: "owner" })])).toEqual([]);
  });

  it("a reader device of a co-publisher marks the group id, never writes the role", () => {
    expect(plan([local()], [live({ role: "co-publisher", publisherGroupIdHex: GROUP })])).toEqual([
      { kind: "co-publisher-mark", idHex: ID, groupIdHex: GROUP },
    ]);
  });

  it("the register's read key is kept; a read key the register lacks is written", () => {
    expect(plan([local()], [live({ readKey: READ })])).toEqual([{ kind: "read-key", idHex: ID, readKey: READ }]);
    const steps = plan([local({ readKey: READ })], [live()]);
    expect(steps[0]).toMatchObject({ kind: "write", value: { readKey: READ, role: "subscriber" } });
  });

  it("a demoted co-publisher device writes the subscriber role", () => {
    const steps = plan([local()], [live({ role: "co-publisher", publisherGroupIdHex: GROUP })], { [ID]: "demoted" });
    expect(steps).toEqual([
      {
        kind: "write",
        idHex: ID,
        value: {
          role: "subscriber",
          subscriberSecret: KEY_A,
          readKey: new Uint8Array(0),
          publisherGroupIdHex: "",
          ownerAccountKey: new Uint8Array(0),
          name: "Daily news",
          handle: "news",
        },
      },
      { kind: "intent-done", idHex: ID },
    ]);
  });

  it("a name or handle the register refuses is not carried", () => {
    expect(registerName("  News  ")).toBe("News");
    expect(registerName("a\nb")).toBe("");
    expect(registerName("x".repeat(256))).toBe("");
    expect(registerHandle("@news_1")).toBe("news_1");
    expect(registerHandle("No")).toBe("");
  });
});

describe("a reader device of an owner or a publisher (UX second-device.md 4a)", () => {
  it("is decided by the register role and group presence", () => {
    expect(readerDeviceRole({ listed: true, publishing: false, registerRole: "owner" })).toBe("owner");
    expect(readerDeviceRole({ listed: true, publishing: false, registerRole: "co-publisher" })).toBe("publisher");
    expect(readerDeviceRole({ listed: true, publishing: true, registerRole: "owner" })).toBeNull();
    expect(readerDeviceRole({ listed: true, publishing: false, registerRole: "subscriber" })).toBeNull();
    expect(readerDeviceRole({ listed: false, publishing: false, registerRole: "owner" })).toBeNull();
  });

  it("shows a subscriber's info with an empty exit block", () => {
    const subscriber = ["handle", "channel-description", "subscribers", "members-can-find-me", "unsubscribe"];
    expect(readerDeviceSections(subscriber, "owner")).toEqual([
      "handle",
      "channel-description",
      "subscribers",
      "members-can-find-me",
    ]);
    expect(readerDeviceSections(subscriber, "publisher")).not.toContain("unsubscribe");
    expect(readerDeviceSections(subscriber, null)).toEqual(subscriber);
  });
});
