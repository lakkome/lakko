// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The words on a feed item (`docs/ux/feed.md` sections 3 and 7), kept out of
 * the component so they are tested and stay one sentence each.
 */

import type { PersistedMessage } from "../chat/message-log-store";
import { t } from "../l10n";
import type { FeedCandidateRef } from "./feed-model";

/** The presets the switcher shows, in order (UX spec section 4). Discover
 *  joins them with ADR-0059. */
export type FeedPresetKey = "latest" | "popular" | "for-me";

export const FEED_PRESETS: ReadonlyArray<{ readonly key: FeedPresetKey; readonly labelKey: string }> = [
  { key: "latest", labelKey: "feed.preset.latest" },
  { key: "popular", labelKey: "feed.preset.popular" },
  { key: "for-me", labelKey: "feed.preset.for_me" },
];

/**
 * The quiet line saying why an item is where it is, or `null` when recency
 * alone put it there. Mentions and replies to the person win over
 * popularity, because they are about the reader.
 */
export function whyLine(ref: FeedCandidateRef, preset: FeedPresetKey): string | null {
  if (ref.mentionsMe) return t("feed.why.mentions");
  if (ref.repliesToMe) return t("feed.why.reply");
  if (preset === "popular" && (ref.message.reactions?.length ?? 0) > 0) {
    return t("feed.why.popular", { source: ref.source.title });
  }
  return null;
}

/** What stands in for text when a message carries only attachments. */
export function attachmentLine(message: PersistedMessage): string | null {
  const n = message.attachments.length;
  if (n === 0) return null;
  if (n > 1) return t("feed.attachments.count", { count: n });
  const a = message.attachments[0];
  switch (a?.kind) {
    case "voice":
      return t("chat.media.voice_message");
    case "video_note":
      return t("chat.media.video_message");
    case "gif":
      return t("chat.media.gif");
    default:
      break;
  }
  const mime = a?.mime ?? "";
  if (mime.startsWith("image/")) return t("chat.media.photo");
  if (mime.startsWith("video/")) return t("chat.media.video");
  return t("chat.media.file");
}

/**
 * The card's text line (`docs/ux/feed.md` section 3). A message with text
 * shows its text; with none, the attachments no bubble draws are named, so a
 * file beside a photo is not lost, and a media-only message has no line.
 */
export function cardBody(
  message: PersistedMessage,
  drawsAsBubble: (attachment: PersistedMessage["attachments"][number]) => boolean,
): string {
  if (message.text.trim() !== "") return message.text;
  const files = message.attachments.filter((a) => !drawsAsBubble(a));
  return attachmentLine({ ...message, attachments: files }) ?? "";
}

/** "3 replies", "1 reply". */
export function repliesLabel(count: number): string {
  return t("feed.replies.count", { count });
}
