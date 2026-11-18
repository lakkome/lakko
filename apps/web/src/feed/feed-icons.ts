// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A feed's icon (`docs/ux/feed.md` section 4.1): one of twelve, by index.
 * The rule file carries the index, so a shared feed keeps its icon; the
 * meaning of each index is fixed by the spec and each client maps it to its
 * own symbols. An index this client does not know draws the default.
 *
 * Presets keep fixed icons of their own; they are not editable and are not
 * in the rule, so they are listed here too, by preset key.
 */

import type { IconName } from "../ui/icons";
import type { FeedPresetKey } from "./feed-copy";

/** The twelve, in index order (UX spec 4.1 table). */
export const FEED_ICONS: ReadonlyArray<{ readonly name: IconName; readonly labelKey: string }> = [
  { name: "feed", labelKey: "feed.icon.feed" },
  { name: "star", labelKey: "feed.icon.star" },
  { name: "heart", labelKey: "feed.icon.heart" },
  { name: "bolt", labelKey: "feed.icon.lightning" },
  { name: "book", labelKey: "feed.icon.book" },
  { name: "code", labelKey: "feed.icon.code" },
  { name: "music-note", labelKey: "feed.icon.music" },
  { name: "camera", labelKey: "feed.icon.camera" },
  { name: "globe", labelKey: "feed.icon.globe" },
  { name: "chat", labelKey: "feed.icon.speech_bubble" },
  { name: "bell", labelKey: "feed.icon.bell" },
  { name: "flag", labelKey: "feed.icon.flag" },
];

/** The default icon's index. */
export const DEFAULT_FEED_ICON = 0;

/** The icon to draw for a rule's index; unknown -> the default. */
export function feedIconName(index: number): IconName {
  const entry = Number.isInteger(index) ? FEED_ICONS[index] : undefined;
  return (entry ?? FEED_ICONS[DEFAULT_FEED_ICON] ?? { name: "feed" as const }).name;
}

/** The presets' fixed icons (chosen: the spec fixes that they have one,
 *  not which). */
export const PRESET_ICONS: Readonly<Record<FeedPresetKey | "discover", IconName>> = {
  latest: "feed",
  popular: "bolt",
  "for-me": "person",
  discover: "globe",
};
