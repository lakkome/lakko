// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Follow-up decision 2 (2026-11-13): a channel orphaned by a key reset, or
 * removed with the orphan "Remove from this device", is never added back by
 * the channel-subscription register and never written to it.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { MemoryStorage } from "../test/fake-storage";
import {
  channelsLeftAloneByRegister,
  clearOrphanedByResetForAccount,
  forgetOrphanedByReset,
  loadOrphanedByReset,
  loadRemovedOrphans,
  recordOrphanedByReset,
} from "./orphaned-by-reset";
import { planChannelSubscriptions } from "./channel-subscription-plan";
import type { ChannelSubscriptionView } from "./channel-subscription-register";

const ACCT = "YWNjb3VudA==";
const ID = "6b".repeat(16);

beforeEach(() => {
  (globalThis as unknown as Record<string, unknown>)["localStorage"] = new MemoryStorage();
});

afterEach(() => {
  delete (globalThis as unknown as Record<string, unknown>)["localStorage"];
});

const live: ChannelSubscriptionView = {
  channelIdHex: ID,
  hlc: 1n,
  subscribed: true,
  role: "owner",
  subscriberSecret: new Uint8Array(32).fill(1),
  readKey: new Uint8Array(0),
  publisherGroupIdHex: "",
  name: "",
  handle: "",
};

/** The runtime's filter, then the plan. */
function planHere(orphaned: ReadonlySet<string>): ReturnType<typeof planChannelSubscriptions> {
  const alone = channelsLeftAloneByRegister(ACCT, orphaned);
  return planChannelSubscriptions({
    local: [],
    held: new Map([[ID, live]].filter(([id]) => !alone.has(id as string)) as [string, ChannelSubscriptionView][]),
    intents: new Map(),
    ending: new Set(),
    freshSecret: () => new Uint8Array(32),
  });
}

describe("orphaned channels and the register", () => {
  it("an orphan is left alone; removed from this device, it stays removed", () => {
    expect(planHere(new Set())).toEqual([{ kind: "add", idHex: ID, register: live }]);
    recordOrphanedByReset(ACCT, [ID]);
    expect(planHere(loadOrphanedByReset(ACCT))).toEqual([]);
    const after = forgetOrphanedByReset(ACCT, ID);
    expect(after.has(ID)).toBe(false);
    expect(loadRemovedOrphans(ACCT).has(ID)).toBe(true);
    // The live register would add it back; the removal record stops it.
    expect(planHere(after)).toEqual([]);
  });

  it("Forget device drops the removal record too", () => {
    recordOrphanedByReset(ACCT, [ID]);
    forgetOrphanedByReset(ACCT, ID);
    clearOrphanedByResetForAccount(ACCT);
    expect(loadRemovedOrphans(ACCT).size).toBe(0);
  });
});
