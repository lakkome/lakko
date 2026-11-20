// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 step 5 - what a channel moderators removed still shows on this
 * device (`docs/ux/reporting-and-moderation.md` section 3): the posts already
 * here, nothing that writes. Pure, so the conversation and the info decide
 * from tested rules rather than in the views.
 */

import type { ChannelPostSlot } from "../feed/channel-post-log";
import type { ChannelPostView } from "./api";
import type { PostDecode } from "./post-decode";

/** What the conversation offers for a channel: composing, reacting and
 *  commenting, the post menu's edit and delete, a report of a post. */
export interface ChannelWriteControls {
  readonly composer: boolean;
  readonly reactAndComment: boolean;
  readonly editDelete: boolean;
  readonly reportPost: boolean;
}

/** The controls offered, from the role and the two states that withhold
 *  them: the account is suspended (ADR-0098 step 3), or the channel was
 *  removed (step 5). */
export function channelWriteControls(input: {
  readonly canPublish: boolean;
  readonly reading: boolean;
  readonly accountSuspended: boolean;
  readonly removed: boolean;
}): ChannelWriteControls {
  if (input.removed) return { composer: false, reactAndComment: false, editDelete: false, reportPost: false };
  return {
    composer: input.canPublish && !input.accountSuspended,
    reactAndComment: input.reading && !input.accountSuspended,
    editDelete: input.canPublish,
    reportPost: !input.canPublish,
  };
}

/**
 * The posts the channel-post log holds for a channel, as the timeline takes
 * them: one page and its decodes (text only, since the log keeps text). Used
 * when a removed channel is opened with nothing fetched in this view.
 */
export function heldPostsOf(slot: ChannelPostSlot | undefined): {
  readonly posts: readonly ChannelPostView[];
  readonly decoded: ReadonlyMap<number, PostDecode>;
} {
  const posts: ChannelPostView[] = [];
  const decoded = new Map<number, PostDecode>();
  for (const p of slot?.posts ?? []) {
    posts.push({ cursor: p.cursor, serverTsMs: p.serverTsMs, payloadB64: "" });
    decoded.set(p.cursor, { kind: "text", text: p.text });
  }
  return { posts, decoded };
}
