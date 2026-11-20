// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0098 — a deleted one-to-one has no composer, no call buttons, no
 *  Reply or React, and sends no typing or read markers. */
import { describe, expect, it } from "vitest";

import { chatCallAffordance } from "../calls/group-call-host";
import { deletedConversationSurface } from "./deleted-surface";

describe("a one-to-one whose other person deleted their account", () => {
  it("offers no composer, no call, no reply, no reaction, no typing", () => {
    expect(deletedConversationSurface(true, true)).toEqual({
      composer: false,
      callButtons: false,
      reply: false,
      react: false,
      ephemeral: false,
    });
  });

  it("an active one-to-one and any group keep everything", () => {
    const open = { composer: true, callButtons: true, reply: true, react: true, ephemeral: true };
    expect(deletedConversationSurface(true, false)).toEqual(open);
    expect(deletedConversationSurface(false, true)).toEqual(open);
  });

  it("the header's call buttons are hidden", () => {
    const input = { membershipEnded: false, callInProgress: false, liveCallInThisChat: false, isGroup: false, people: 2 };
    expect(chatCallAffordance(input)).toBe("start-one-to-one");
    expect(chatCallAffordance({ ...input, peerDeleted: true })).toBe("hidden");
    // A group is not a one-to-one: a deleted member does not end its calls.
    expect(chatCallAffordance({ ...input, isGroup: true, people: 3, peerDeleted: true })).toBe("start-group");
  });
});
