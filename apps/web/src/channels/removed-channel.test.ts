// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0099 step 5 - what a channel moderators removed still offers: the posts
 * held on this device, readable; no composer, reactions, comments, edit or
 * delete; in the info only the @name, the description, the find-me switch
 * and leaving.
 */
import { describe, expect, it } from "vitest";

import { REMOVED_CHANNEL_SECTIONS, sectionsWhileRemoved, type InfoSection } from "../chat/conversation-info";
import { channelTimeline } from "./channel-timeline";
import { channelWriteControls, heldPostsOf } from "./removed-channel";

describe("a removed channel's controls", () => {
  it("withholds every write for a publisher and a subscriber alike", () => {
    for (const canPublish of [true, false]) {
      expect(
        channelWriteControls({ canPublish, reading: true, accountSuspended: false, removed: true }),
      ).toEqual({ composer: false, reactAndComment: false, editDelete: false, reportPost: false });
    }
  });

  it("offers what it offered before when the channel answers again", () => {
    expect(channelWriteControls({ canPublish: true, reading: true, accountSuspended: false, removed: false })).toEqual({
      composer: true,
      reactAndComment: true,
      editDelete: true,
      reportPost: false,
    });
    expect(channelWriteControls({ canPublish: false, reading: true, accountSuspended: false, removed: false })).toEqual({
      composer: false,
      reactAndComment: true,
      editDelete: false,
      reportPost: true,
    });
    // ADR-0099 step 3 still applies on its own.
    expect(
      channelWriteControls({ canPublish: true, reading: true, accountSuspended: true, removed: false }).composer,
    ).toBe(false);
  });

  it("keeps the posts on this device readable", () => {
    const held = heldPostsOf({
      cursor: 3,
      posts: [
        { cursor: 1, serverTsMs: 1000, text: "first" },
        { cursor: 3, serverTsMs: 3000, text: "third" },
      ],
    });
    const timeline = channelTimeline(held.posts, held.decoded);
    expect(timeline.posts.map((p) => [p.cursor, p.state, p.text])).toEqual([
      [1, "ok", "first"],
      [3, "ok", "third"],
    ]);
    expect(heldPostsOf(undefined).posts).toHaveLength(0);
  });

  it("leaves in the info only the @name, the description, the find-me switch and leaving", () => {
    const all: InfoSection[] = [
      "handle",
      "channel-description",
      "rename-group",
      "invite-people",
      "invite-link",
      "subscribers",
      "admins",
      "reactions",
      "group-type",
      "members-can-find-me",
      "report",
      "unsubscribe",
      "delete-group",
    ];
    expect(sectionsWhileRemoved(all, true)).toEqual(["handle", "channel-description", "members-can-find-me", "unsubscribe"]);
    expect(sectionsWhileRemoved(all, false)).toEqual(all);
    expect(REMOVED_CHANNEL_SECTIONS).not.toContain("invite-link");
  });

  it("gives the owner the subscriber's leave control in place of Delete", () => {
    const owner: InfoSection[] = [
      "handle",
      "channel-description",
      "rename-group",
      "invite-people",
      "invite-link",
      "subscribers",
      "admins",
      "reactions",
      "group-type",
      "members-can-find-me",
      "delete-group",
    ];
    const shown = sectionsWhileRemoved(owner, true);
    expect(shown).toEqual(["handle", "channel-description", "members-can-find-me", "unsubscribe"]);
    expect(shown).not.toContain("delete-group");
    // Not removed: the owner's own rule, no leave control added.
    expect(sectionsWhileRemoved(owner, false)).not.toContain("unsubscribe");
  });
});
