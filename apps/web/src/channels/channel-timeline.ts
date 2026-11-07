// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a channel's timeline shows (ADR-0087 sections 12 and 13; UX
 * `docs/ux/group-and-channel-privacy.md` section 13): the decoded posts in
 * cursor order with their edit and delete ops applied, and whether older
 * posts exist that this device cannot read. Pure: the view feeds it the
 * fetched page and the decode outcomes.
 *
 * - An edit (a later post naming an earlier cursor) replaces the text and
 *   marks the post edited; the newest edit wins.
 * - A delete removes the post for everyone.
 * - A metadata record, a ban list, an op and a malformed record are never a
 *   post.
 * - Posts with no key on this device at the start of the timeline fold into
 *   one line at the top ("Earlier posts aren't available."); one in the middle
 *   is shown as unavailable.
 */
import type { AttachmentRefBytes } from "../chat/attachments";
import type { PostDecode } from "./post-decode";

/** One fetched post as the timeline sees it. */
export interface TimelineInput {
  readonly cursor: number;
  readonly serverTsMs: number;
}

/** One card of the timeline. */
export interface TimelinePost {
  readonly cursor: number;
  readonly serverTsMs: number;
  /** "loading" until its decode finished; "unavailable" when it cannot be
   *  read on this device; "ok" otherwise. */
  readonly state: "loading" | "unavailable" | "ok";
  readonly text: string;
  readonly attachments: readonly AttachmentRefBytes[];
  readonly edited: boolean;
}

export interface ChannelTimeline {
  readonly posts: readonly TimelinePost[];
  /** Older posts exist that this device cannot read. */
  readonly earlierUnavailable: boolean;
}

/** Build the timeline from a page (any order) and the decode per cursor. */
export function channelTimeline(
  page: readonly TimelineInput[],
  decoded: ReadonlyMap<number, PostDecode>,
): ChannelTimeline {
  const ordered = [...page].sort((a, b) => a.cursor - b.cursor);
  const edits = new Map<number, string>();
  const deleted = new Set<number>();
  for (const p of ordered) {
    const d = decoded.get(p.cursor);
    if (d?.kind !== "op") continue;
    // An op only reaches back: an op naming itself or a later cursor is junk.
    if (d.body.target >= p.cursor) continue;
    if (d.body.kind === "delete") deleted.add(d.body.target);
    else edits.set(d.body.target, d.body.text);
  }

  const posts: TimelinePost[] = [];
  let earlierUnavailable = false;
  for (const p of ordered) {
    if (deleted.has(p.cursor)) continue;
    const d = decoded.get(p.cursor);
    if (d === undefined) {
      posts.push({ cursor: p.cursor, serverTsMs: p.serverTsMs, state: "loading", text: "", attachments: [], edited: false });
      continue;
    }
    switch (d.kind) {
      case "meta":
      case "ban-list":
      case "op":
      case "dropped":
        continue;
      case "older-key":
      case "unreadable":
        // Before the first post shown: one line at the top instead of cards.
        if (posts.length === 0) {
          earlierUnavailable = true;
          continue;
        }
        posts.push({ cursor: p.cursor, serverTsMs: p.serverTsMs, state: "unavailable", text: "", attachments: [], edited: false });
        continue;
      case "text": {
        const edit = edits.get(p.cursor);
        posts.push({
          cursor: p.cursor,
          serverTsMs: p.serverTsMs,
          state: "ok",
          text: edit ?? d.text,
          attachments: d.body?.attachments ?? [],
          edited: edit !== undefined,
        });
        continue;
      }
    }
  }
  return { posts, earlierUnavailable };
}

/**
 * The day separator before each post (as in chats): the post's day label when
 * it is the first post or falls on another local day than the post before it,
 * else `null`. Index for index with `posts`; a separator always PRECEDES the
 * first post of its day.
 */
export function channelDayLabels(
  posts: readonly { readonly serverTsMs: number }[],
  label: (iso: string) => string,
  same: (a: string, b: string) => boolean,
): (string | null)[] {
  const out: (string | null)[] = [];
  let prev: string | null = null;
  for (const p of posts) {
    const iso = new Date(p.serverTsMs).toISOString();
    out.push(prev === null || !same(prev, iso) ? label(iso) : null);
    prev = iso;
  }
  return out;
}
