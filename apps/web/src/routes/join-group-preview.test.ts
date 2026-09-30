// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/group-invite-page.md section 3: the headline says who invites you
 * and to what; a missing name is never invented; the count is the roster at
 * mint time.
 */
import { describe, expect, it } from "vitest";
import {
  inviteHeadline,
  invitedByLine,
  memberCountLine,
} from "./join-group-preview";

describe("inviteHeadline", () => {
  it("names the inviter and the group, with the count on its own line", () => {
    expect(
      inviteHeadline({ inviterName: "Anna", groupName: "Weekend hikers", memberCount: 7 }),
    ).toEqual({ lead: "Anna invites you to", title: "Weekend hikers", members: "7 members" });
  });

  it("reads `You are invited to` when the inviter has no name", () => {
    expect(inviteHeadline({ groupName: "Weekend hikers", memberCount: 3 })).toEqual({
      lead: "You are invited to",
      title: "Weekend hikers",
      members: "3 members",
    });
  });

  it("folds `a group` into the sentence and drops the title line when the group has no name", () => {
    expect(inviteHeadline({ inviterName: "Anna", memberCount: 2 })).toEqual({
      lead: "Anna invites you to a group",
      title: null,
      members: "2 members",
    });
  });

  it("reads `You are invited to a group` when both names are missing, keeping the count", () => {
    expect(inviteHeadline({ memberCount: 4 })).toEqual({
      lead: "You are invited to a group",
      title: null,
      members: "4 members",
    });
  });

  it("treats a blank name as missing", () => {
    expect(inviteHeadline({ inviterName: "  ", groupName: "\t", memberCount: 1 })).toEqual({
      lead: "You are invited to a group",
      title: null,
      members: "1 member",
    });
  });

  it("singular for one member, plural otherwise", () => {
    expect(memberCountLine(1)).toBe("1 member");
    expect(memberCountLine(2)).toBe("2 members");
    expect(memberCountLine(0)).toBe("0 members");
    expect(inviteHeadline({ inviterName: "Anna", groupName: "Solo", memberCount: 1 }).members).toBe(
      "1 member",
    );
  });

  it("without a preview reads exactly the no-preview card, with no count line", () => {
    expect(inviteHeadline(null)).toEqual({
      lead: "You are invited to a group",
      title: null,
      members: null,
    });
  });

  it("shows names as written, unlinked and unmarked", () => {
    const h = inviteHeadline({ inviterName: "@anna", groupName: "<b>x</b>", memberCount: 2 });
    expect(h.lead).toBe("@anna invites you to");
    expect(h.title).toBe("<b>x</b>");
  });
});

describe("invitedByLine", () => {
  it("prefixes the fingerprint with `Invited by`", () => {
    expect(invitedByLine("12345 67890")).toBe("Invited by 12345 67890");
  });
});
