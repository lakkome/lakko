// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { channelLinkLead } from "./channel-link-lead";

describe("the first line of a channel's link page (UX 7.2)", () => {
  it("names the inviter when the link carries the name", () => {
    expect(channelLinkLead({ publisherInvite: false, inviterName: " Seventeen A " })).toBe("Seventeen A invites you to");
  });

  it("says who is invited when the link names nobody", () => {
    expect(channelLinkLead({ publisherInvite: false })).toBe("You are invited to");
    expect(channelLinkLead({ publisherInvite: false, inviterName: "  " })).toBe("You are invited to");
  });

  it("leaves a publisher invitation to its own line", () => {
    expect(channelLinkLead({ publisherInvite: true, inviterName: "Anna" })).toBeNull();
  });
});
