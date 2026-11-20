// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A channel Welcome that came before this device applied its removal is held,
 * not lost (walk follow-up of 2026-11-14): the store's bounds and the
 * per-pass decision.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { MemoryStorage } from "../test/fake-storage";
import {
  HELD_CHANNEL_WELCOME_MAX_AGE_MS,
  HELD_CHANNEL_WELCOMES_PER_CHANNEL,
  heldChannelWelcomeAction,
  heldChannelWelcomeExpired,
  loadHeldChannelWelcomes,
  saveHeldChannelWelcomes,
  withHeldChannelWelcome,
} from "./held-channel-welcomes";

beforeEach(() => {
  (globalThis as unknown as Record<string, unknown>)["localStorage"] = new MemoryStorage();
});
afterEach(() => {
  delete (globalThis as unknown as Record<string, unknown>)["localStorage"];
});

describe("the held list", () => {
  it("holds an identical Welcome once and keeps a channel's newest few", () => {
    let list = withHeldChannelWelcome([], { channelIdHex: "aa", welcomeB64: "w1", heldAtMs: 1 });
    list = withHeldChannelWelcome(list, { channelIdHex: "aa", welcomeB64: "w1", heldAtMs: 9 });
    expect(list).toEqual([{ channelIdHex: "aa", welcomeB64: "w1", heldAtMs: 1 }]);
    for (let i = 2; i <= HELD_CHANNEL_WELCOMES_PER_CHANNEL + 2; i += 1) {
      list = withHeldChannelWelcome(list, { channelIdHex: "aa", welcomeB64: `w${i}`, heldAtMs: i });
    }
    list = withHeldChannelWelcome(list, { channelIdHex: "bb", welcomeB64: "x", heldAtMs: 1 });
    const aa = list.filter((e) => e.channelIdHex === "aa").map((e) => e.welcomeB64);
    expect(aa).toHaveLength(HELD_CHANNEL_WELCOMES_PER_CHANNEL);
    expect(aa).not.toContain("w1");
    expect(list.filter((e) => e.channelIdHex === "bb")).toHaveLength(1);
  });

  it("round-trips through the store, per account; a damaged row reads as none", () => {
    const list = [{ channelIdHex: "aa", welcomeB64: "w", heldAtMs: 5 }];
    expect(saveHeldChannelWelcomes("acct", list)).toBe(true);
    expect(loadHeldChannelWelcomes("acct")).toEqual(list);
    expect(loadHeldChannelWelcomes("other")).toEqual([]);
    localStorage.setItem("lakko.channels.held-welcomes.v1.acct", "{not json");
    expect(loadHeldChannelWelcomes("acct")).toEqual([]);
    expect(saveHeldChannelWelcomes("acct", [])).toBe(true);
    expect(localStorage.getItem("lakko.channels.held-welcomes.v1.acct")).toBeNull();
  });

  it("expires after the chosen bound; a clock that moved back keeps it", () => {
    const e = { channelIdHex: "aa", welcomeB64: "w", heldAtMs: 1_000 };
    expect(heldChannelWelcomeExpired(e, 1_000 + HELD_CHANNEL_WELCOME_MAX_AGE_MS)).toBe(false);
    expect(heldChannelWelcomeExpired(e, 1_001 + HELD_CHANNEL_WELCOME_MAX_AGE_MS)).toBe(true);
    expect(heldChannelWelcomeExpired(e, 0)).toBe(false);
  });
});

describe("what a pass does with a held Welcome", () => {
  it("keeps it while the group is active and unsuspected, tries it once not, drops it when the channel is gone or it aged", () => {
    const base = { channelStillOurs: true, heldGroup: "active" as const, suspected: false, expired: false };
    expect(heldChannelWelcomeAction(base)).toBe("keep");
    expect(heldChannelWelcomeAction({ ...base, heldGroup: "inactive" })).toBe("try");
    expect(heldChannelWelcomeAction({ ...base, heldGroup: "none" })).toBe("try");
    expect(heldChannelWelcomeAction({ ...base, suspected: true })).toBe("try");
    expect(heldChannelWelcomeAction({ ...base, expired: true })).toBe("drop");
    expect(heldChannelWelcomeAction({ ...base, heldGroup: "inactive", expired: true })).toBe("drop");
    expect(heldChannelWelcomeAction({ ...base, channelStillOurs: false, heldGroup: "inactive" })).toBe("drop");
  });
});
