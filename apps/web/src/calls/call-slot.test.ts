// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { callBusyMessage, callSlotOccupant } from "./call-slot";

const FREE = { oneToOneInCall: false, groupCallActive: false, linkCallActive: false };

describe("callSlotOccupant", () => {
  it("is free when no call of any kind is running", () => {
    expect(callSlotOccupant(FREE)).toBeNull();
  });

  it("names whichever single call holds the slot", () => {
    expect(callSlotOccupant({ ...FREE, oneToOneInCall: true })).toBe("one-to-one");
    expect(callSlotOccupant({ ...FREE, groupCallActive: true })).toBe("group");
    expect(callSlotOccupant({ ...FREE, linkCallActive: true })).toBe("link");
  });

  it("is deterministic even if a bug let two kinds run at once", () => {
    expect(
      callSlotOccupant({ oneToOneInCall: true, groupCallActive: true, linkCallActive: true }),
    ).toBe("one-to-one");
    expect(callSlotOccupant({ ...FREE, groupCallActive: true, linkCallActive: true })).toBe(
      "group",
    );
  });
});

describe("callBusyMessage", () => {
  it("names the call that is running and what to do about it", () => {
    expect(callBusyMessage("group", "join")()).toBe(
      "You are already in a group call. End it before you join another one.",
    );
    expect(callBusyMessage("link", "start")()).toBe(
      "You are already in a call from a link. End it before you start another one.",
    );
    expect(callBusyMessage("one-to-one", "join")()).toContain("already in a call.");
  });
});
