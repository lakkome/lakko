// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The words on a feed item (`docs/ux/feed.md` sections 3 and 7), kept out of
 * the component so they are tested and stay one sentence each.
 */

import type { PersistedMessage } from "../chat/message-log-store";
import type { FeedCandidateRef } from "./feed-model";

/** The presets the switcher shows, in order (UX spec section 4). Discover
 *  joins them with ADR-0059. */
export type FeedPresetKey = "latest" | "popular" | "for-me";

export const FEED_PRESETS: ReadonlyArray<{ readonly key: FeedPresetKey; readonly label: string }> = [
  { key: "latest", label: "Latest" },
  { key: "popular", label: "Popular" },
  { key: "for-me", label: "For me" },
];

/**
 * The quiet line saying why an item is where it is, or `null` when recency
 * alone put it there. Mentions and replies to the person win over
 * popularity, because they are about the reader.
 */
export function whyLine(ref: FeedCandidateRef, preset: FeedPresetKey): string | null {
  if (ref.mentionsMe) return "Mentions you";
  if (ref.repliesToMe) return "Reply to your message";
  if (preset === "popular" && (ref.message.reactions?.length ?? 0) > 0) {
    return `Popular in ${ref.source.title}`;
  }
  return null;
}

/** What stands in for text when a message carries only attachments. */
export function attachmentLine(message: PersistedMessage): string | null {
  const n = message.attachments.length;
  if (n === 0) return null;
  if (n > 1) return `${n} attachments`;
  const a = message.attachments[0];
  switch (a?.kind) {
    case "voice":
      return "Voice message";
    case "video_note":
      return "Video message";
    case "gif":
      return "GIF";
    default:
      break;
  }
  const mime = a?.mime ?? "";
  if (mime.startsWith("image/")) return "Photo";
  if (mime.startsWith("video/")) return "Video";
  return "File";
}

/** "3 replies", "1 reply". */
export function repliesLabel(count: number): string {
  return count === 1 ? "1 reply" : `${count} replies`;
}
