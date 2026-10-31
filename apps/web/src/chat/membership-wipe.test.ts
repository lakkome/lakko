// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 section 9 — the list of what a group leaves on this device.
 *
 * Each store is run for real where it has no IndexedDB under it: a group goes,
 * another group of the same account stays. The stores that need IndexedDB are
 * stubbed and their call is what is asserted.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

import { MemoryStorage } from "../test/fake-storage";

const calls: string[] = [];
const state = { logFails: false };

vi.mock("../auth/key-store", () => ({ getDeviceKey: async () => ({ fake: "key" }) }));
vi.mock("./message-log-store", () => ({
  deleteGroupFromMessageLog: async (_a: string, _k: unknown, hex: string) => {
    calls.push(`message-log ${hex}`);
    if (state.logFails) throw new Error("log write failed");
    return true;
  },
}));
vi.mock("./commit-outbox", () => ({
  deleteOutboxForGroup: async (_a: string, g: string) => {
    calls.push(`outbox ${g}`);
  },
}));
vi.mock("../push/group-routing-store", () => ({
  deleteGroupRouting: async (_a: string, g: string) => {
    calls.push(`routing ${g}`);
  },
}));
vi.mock("../feed/feed-marks-store", () => ({
  updateFeedMarks: async (_a: string, fn: (m: unknown) => unknown) => {
    calls.push("feed-marks");
    return fn({ hidden: [], looked: {}, channelRead: {} });
  },
}));
vi.mock("../contacts/carried-copies", () => ({
  dropCarriedCopies: async (_a: string, key: string) => {
    calls.push(`carried ${key}`);
    return false;
  },
}));
vi.mock("../ds/registrar-seeds", () => ({
  registrarSeeds: {
    delete: async (g: string) => {
      calls.push(`registrar ${g}`);
    },
  },
}));
vi.mock("../ds/root-handover", () => ({
  clearHandoverOwed: (_a: string, g: string) => {
    calls.push(`handover ${g}`);
  },
  pendingRootSeeds: {
    delete: async (g: string) => {
      calls.push(`pending-root ${g}`);
    },
  },
}));
vi.mock("../reach/reach-service", () => ({
  reachSession: () => ({
    updateReceivedGrants: async () => {
      calls.push("reach");
    },
  }),
}));
vi.mock("./held-send-queue", () => ({
  heldSends: { take: () => [{ heldAtMs: 0, payload: {} }] },
  persistHeldSends: async (a: string) => {
    calls.push(`held ${a}`);
  },
}));

import { recordBeaconObservation, loadBeaconObservations } from "./committer-beacon";
import { readKnockCursor, writeKnockCursor } from "./knock-cursor";
import { loadLeaveRequestedAt, recordLeaveRequested } from "./membership-ended";
import { groupWipeSteps, runWipeSteps, type GroupWipeContexts } from "./membership-wipe";
import { loadLastObservedOwner, rememberObservedOwner } from "./ownership-transfer";
import { isOwnPublished, markOwnPublished } from "./own-published";
import { recordRemovedDevices, removedDevices } from "./device-readmission";
import { loadMemberBaseline, observeMembership } from "../profile/membership-baseline";
import { lastProfileRequestAt, recordProfileRequest } from "../profile/profile-request";
import { isRootHeld, markRootHeld } from "../ds/root-held";

const ACCOUNT = "acct";
const GROUP = "gone";
const OTHER = "stays";

function contexts(): GroupWipeContexts {
  const note = (what: string) => (g: string) => {
    calls.push(`${what} ${g}`);
  };
  return {
    forgetPins: async (g) => note("pins")(g),
    forgetGroupName: async (g) => note("name")(g),
    forgetGroupClosed: async (g) => note("closed")(g),
    forgetReadWatermarks: note("watermarks"),
    forgetSharedChannels: async (g) => note("shared")(g),
    forgetCallLog: note("call-log"),
  };
}

function steps() {
  return groupWipeSteps(
    { accountIdB64: ACCOUNT, groupIdB64: GROUP, groupIdHex: "aa", conversationHex: "root" },
    contexts(),
  );
}

beforeEach(() => {
  calls.length = 0;
  state.logFails = false;
  vi.stubGlobal("localStorage", new MemoryStorage());
  vi.stubGlobal("sessionStorage", new MemoryStorage());
});

describe("the list", () => {
  it("names every store a group leaves behind, each once", () => {
    const names = steps().map((s) => s.name);
    expect(new Set(names).size).toBe(names.length);
    expect(names).toEqual([
      "message-log",
      "held-sends",
      "carried-copies",
      "pins",
      "group-name",
      "group-closed",
      "read-watermarks",
      "shared-channels",
      "call-log",
      "group-calls",
      "own-published",
      "commit-outbox",
      "push-routing",
      "pending-requests",
      "unapplied-requests",
      "own-requests",
      "armed-retries",
      "leave-requested",
      "committer-beacons",
      "last-observed-owner",
      "readmission-removed",
      "knock-cursor",
      "continuity",
      "member-baseline",
      "profile-request",
      "registrar-seed",
      "pending-root-seed",
      "ds-appointments",
      "ds-registrar-list",
      "ds-root-handover",
      "ds-root-held",
      "reach-grants",
      "feed-marks",
      "restored-known-groups",
    ]);
  });

  it("reaches the stores it does not own, keyed on the group or the conversation", async () => {
    expect(await runWipeSteps(steps())).toEqual([]);
    expect(calls).toEqual(
      expect.arrayContaining([
        "message-log aa",
        "held acct",
        "carried root",
        `pins ${GROUP}`,
        `name ${GROUP}`,
        `closed ${GROUP}`,
        `watermarks ${GROUP}`,
        `shared ${GROUP}`,
        `call-log ${GROUP}`,
        `outbox ${GROUP}`,
        `routing ${GROUP}`,
        `registrar ${GROUP}`,
        `pending-root ${GROUP}`,
        `handover ${GROUP}`,
        "reach",
        "feed-marks",
      ]),
    );
  });

  it("clears each store of this group and no other group's", async () => {
    for (const g of [GROUP, OTHER]) {
      recordLeaveRequested(ACCOUNT, g, 1);
      recordBeaconObservation(ACCOUNT, g, "sender", 1);
      rememberObservedOwner(ACCOUNT, g, "owner");
      recordRemovedDevices(ACCOUNT, g, ["a"], []);
      writeKnockCursor(g, 3, ACCOUNT);
      observeMembership(ACCOUNT, g, new Set(["a"]));
      recordProfileRequest(ACCOUNT, g, 1);
      markRootHeld(ACCOUNT, g);
      markOwnPublished(g, 5);
      sessionStorage.setItem(`lakko:group-calls:${ACCOUNT}:${g}`, "{}");
    }
    await runWipeSteps(steps());

    expect(loadLeaveRequestedAt(ACCOUNT, GROUP)).toBeUndefined();
    expect(loadBeaconObservations(ACCOUNT, GROUP).size).toBe(0);
    expect(loadLastObservedOwner(ACCOUNT, GROUP)).toBeUndefined();
    expect(removedDevices(ACCOUNT, GROUP).size).toBe(0);
    expect(readKnockCursor(GROUP, ACCOUNT)).toBe(0);
    expect(loadMemberBaseline(ACCOUNT, GROUP)).toBeNull();
    expect(lastProfileRequestAt(ACCOUNT, GROUP)).toBeUndefined();
    expect(isRootHeld(ACCOUNT, GROUP)).toBe(false);
    expect(isOwnPublished(GROUP, 5)).toBe(false);

    expect(loadLeaveRequestedAt(ACCOUNT, OTHER)).toBe(1);
    expect(loadBeaconObservations(ACCOUNT, OTHER).size).toBe(1);
    expect(loadLastObservedOwner(ACCOUNT, OTHER)).toBe("owner");
    expect(removedDevices(ACCOUNT, OTHER).size).toBe(1);
    expect(readKnockCursor(OTHER, ACCOUNT)).toBe(3);
    expect(loadMemberBaseline(ACCOUNT, OTHER)).not.toBeNull();
    expect(lastProfileRequestAt(ACCOUNT, OTHER)).toBe(1);
    expect(isRootHeld(ACCOUNT, OTHER)).toBe(true);
    expect(isOwnPublished(OTHER, 5)).toBe(true);
    expect(sessionStorage.getItem(`lakko:group-calls:${ACCOUNT}:${GROUP}`)).toBeNull();
    expect(sessionStorage.getItem(`lakko:group-calls:${ACCOUNT}:${OTHER}`)).toBe("{}");
  });

  it("runs on past a store that failed and names it", async () => {
    state.logFails = true;
    const failed = await runWipeSteps(steps());
    expect(failed).toEqual(["message-log"]);
    expect(calls).toContain(`pins ${GROUP}`);
  });

  it("can be run twice", async () => {
    expect(await runWipeSteps(steps())).toEqual([]);
    expect(await runWipeSteps(steps())).toEqual([]);
  });
});
