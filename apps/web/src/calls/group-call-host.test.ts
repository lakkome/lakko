// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  chatCallAffordance,
  decideGroupCallEpochChange,
  planGroupCallLeave,
  selectGroupCallBanner,
  type ChatCallAffordanceInput,
  type GroupCallBannerInput,
  type GroupCallEpochInput,
  type GroupCallLeaveInput,
} from "./group-call-host";

function leave(over: Partial<GroupCallLeaveInput>): GroupCallLeaveInput {
  return {
    reason: "hungUp",
    sessionFailed: false,
    joinPublished: true,
    initiator: false,
    ownLeaf: 3,
    recordJoinedLeaves: new Set([3]),
    ...over,
  };
}

describe("planGroupCallLeave", () => {
  it("a joined participant who was the last one in publishes Leave then End", () => {
    expect(planGroupCallLeave(leave({}))).toEqual({
      leaveReason: "hungUp",
      publishLeave: true,
      publishEnd: true,
    });
  });

  it("does not end a call somebody else is still in", () => {
    const plan = planGroupCallLeave(leave({ recordJoinedLeaves: new Set([3, 7]) }));
    expect(plan.publishLeave).toBe(true);
    expect(plan.publishEnd).toBe(false);
  });

  it("a joiner whose connection failed before it ever joined tells nobody anything", () => {
    // It must not end a call other members may be about to enter.
    const plan = planGroupCallLeave(
      leave({ joinPublished: false, recordJoinedLeaves: new Set<number>(), reason: "error" }),
    );
    expect(plan.publishLeave).toBe(false);
    expect(plan.publishEnd).toBe(false);
  });

  it("an initiator whose session never connected still owes the End of the call it announced", () => {
    const plan = planGroupCallLeave(
      leave({ joinPublished: false, initiator: true, recordJoinedLeaves: new Set<number>() }),
    );
    expect(plan.publishLeave).toBe(false);
    expect(plan.publishEnd).toBe(true);
  });

  it("a record already gone (someone else ended it) counts as nobody else joined", () => {
    expect(planGroupCallLeave(leave({ recordJoinedLeaves: null })).publishEnd).toBe(true);
  });

  it("a failed session overrides the requested reason", () => {
    expect(planGroupCallLeave(leave({ sessionFailed: true })).leaveReason).toBe("error");
    expect(planGroupCallLeave(leave({ reason: "error" })).leaveReason).toBe("error");
    expect(planGroupCallLeave(leave({})).leaveReason).toBe("hungUp");
  });
});

function epoch(over: Partial<GroupCallEpochInput>): GroupCallEpochInput {
  return {
    callGroupIdB64: "group-a",
    settledGroupIdB64: "group-a",
    groupJoined: true,
    isActive: true,
    keyedEpoch: 5n,
    currentEpoch: 5n,
    ...over,
  };
}

describe("decideGroupCallEpochChange", () => {
  it("rekeys when the call's group drained at a different epoch", () => {
    expect(decideGroupCallEpochChange(epoch({ currentEpoch: 6n }))).toBe("rekey");
  });

  it("rekeys back on a rollback too (the ADR-0040 F1 reset moves the epoch down)", () => {
    expect(decideGroupCallEpochChange(epoch({ keyedEpoch: 6n, currentEpoch: 5n }))).toBe("rekey");
  });

  it("does nothing when the epoch has not moved — a send or a quiet poll drains the lock too", () => {
    expect(decideGroupCallEpochChange(epoch({}))).toBe("none");
  });

  it("ignores a drain of any other group, whatever happened there", () => {
    expect(
      decideGroupCallEpochChange(
        epoch({ settledGroupIdB64: "group-b", currentEpoch: 9n, isActive: false }),
      ),
    ).toBe("none");
  });

  it("does nothing without a live group call", () => {
    expect(decideGroupCallEpochChange(epoch({ callGroupIdB64: null, currentEpoch: 9n }))).toBe(
      "none",
    );
  });

  it("keeps the current key when the epoch cannot be read (fail-closed, no guess)", () => {
    expect(decideGroupCallEpochChange(epoch({ currentEpoch: undefined }))).toBe("none");
  });

  it("rekeys once the epoch becomes readable when it could not be read at start", () => {
    expect(decideGroupCallEpochChange(epoch({ keyedEpoch: null }))).toBe("rekey");
  });

  it("ends the call locally when this device is no longer in the group", () => {
    expect(decideGroupCallEpochChange(epoch({ isActive: false, currentEpoch: 6n }))).toBe(
      "end-locally",
    );
    expect(decideGroupCallEpochChange(epoch({ groupJoined: false }))).toBe("end-locally");
  });

  it("an unreadable membership flag is not an eviction", () => {
    expect(decideGroupCallEpochChange(epoch({ isActive: undefined, currentEpoch: 6n }))).toBe(
      "rekey",
    );
  });
});

function banner(over: Partial<GroupCallBannerInput>): GroupCallBannerInput {
  return {
    live: [
      { groupIdB64: "group-a", callIdHex: "aa" },
      { groupIdB64: "group-b", callIdHex: "bb" },
    ],
    isChatOpen: () => false,
    isMember: () => true,
    inAnyCall: false,
    dismissedCallIds: new Set(),
    ...over,
  };
}

describe("selectGroupCallBanner", () => {
  it("announces the first live call when no chat is open", () => {
    expect(selectGroupCallBanner(banner({}))).toEqual({ groupIdB64: "group-a", callIdHex: "aa" });
  });

  it("leaves a call to its own chat's banner when that chat is open", () => {
    expect(selectGroupCallBanner(banner({ isChatOpen: (g) => g === "group-a" }))).toEqual({
      groupIdB64: "group-b",
      callIdHex: "bb",
    });
  });

  it("announces nothing while this device is in any call", () => {
    expect(selectGroupCallBanner(banner({ inAnyCall: true }))).toBeNull();
  });

  it("skips a dismissed call, and a group this device has left", () => {
    expect(
      selectGroupCallBanner(
        banner({ dismissedCallIds: new Set(["aa"]), isMember: (g) => g !== "group-b" }),
      ),
    ).toBeNull();
  });

  it("announces nothing when there is no live call", () => {
    expect(selectGroupCallBanner(banner({ live: [] }))).toBeNull();
  });
});

function affordance(over: Partial<ChatCallAffordanceInput>): ChatCallAffordanceInput {
  return {
    membershipEnded: false,
    oneToOneInCall: false,
    groupCallActive: false,
    liveCallInThisChat: false,
    memberCount: 2,
    ...over,
  };
}

describe("chatCallAffordance", () => {
  it("a live group call anywhere hides the buttons, so a return to its chat never starts a second one", () => {
    expect(chatCallAffordance(affordance({ groupCallActive: true, memberCount: 5 }))).toBe(
      "hidden",
    );
    expect(
      chatCallAffordance(affordance({ groupCallActive: true, liveCallInThisChat: true })),
    ).toBe("hidden");
  });

  it("a 1x1 call anywhere hides them too", () => {
    expect(chatCallAffordance(affordance({ oneToOneInCall: true }))).toBe("hidden");
  });

  it("hides them once membership has ended", () => {
    expect(
      chatCallAffordance(affordance({ membershipEnded: true, liveCallInThisChat: true })),
    ).toBe("hidden");
  });

  it("joins a call already in progress in this chat, even a two-member one", () => {
    expect(chatCallAffordance(affordance({ liveCallInThisChat: true }))).toBe("join-group");
    expect(chatCallAffordance(affordance({ liveCallInThisChat: true, memberCount: 9 }))).toBe(
      "join-group",
    );
  });

  it("otherwise starts a group call for more than two members and a 1x1 call for two", () => {
    expect(chatCallAffordance(affordance({ memberCount: 3 }))).toBe("start-group");
    expect(chatCallAffordance(affordance({ memberCount: 2 }))).toBe("start-one-to-one");
  });
});
