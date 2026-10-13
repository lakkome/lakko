// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Per-device feed marks (`docs/ux/feed.md` sections 5 and 9 item 8): what
 * this device remembers about reading, apart from the feeds themselves.
 *
 * - `hidden`: conversations hidden from feeds ("Hide from feeds"). Taken out
 *   of every feed and of the suggestions before ranking. Not a chat mute.
 * - `looked`: per feed, the items that were in it the last time the person
 *   left the Feed screen, for the "New" divider.
 * - `channelRead`: per channel, the newest post read, in the feed or in the
 *   channel. A channel's count of new posts is what lies above it.
 *
 * Pure: the codec and every rule are here and tested; the sealed store is
 * `feed-marks-store.ts`. Nothing here leaves the device.
 */
import { currentLanguage, t } from "../l10n";

export const FEED_MARKS_VERSION = 1;
/** Most item keys remembered per feed (chosen): well past a screenful, and
 *  an item further down the list than this is old news anyway. */
export const MAX_LOOKED_KEYS = 2000;
/** Most conversations hidden (chosen): bounds a stored list, not a person. */
export const MAX_HIDDEN = 1000;

export interface FeedMarks {
  /** Conversation ids hidden from feeds: a group's log key or a channel's
   *  32-char hex id, the keys the feed's sources use. */
  readonly hidden: readonly string[];
  /** Feed key -> item keys that were in it when the person last left. */
  readonly looked: { readonly [feedKey: string]: readonly string[] };
  /** Channel id -> cursor of the newest post read. */
  readonly channelRead: { readonly [channelIdHex: string]: number };
}

export function emptyFeedMarks(): FeedMarks {
  return { hidden: [], looked: {}, channelRead: {} };
}

const HEX_RE = /^[0-9a-f]{2,128}$/;
const CHANNEL_RE = /^[0-9a-f]{32}$/;
const FEED_KEY_RE = /^[A-Za-z0-9:_-]{1,64}$/;

/** Lenient per entry: a malformed one is dropped alone. */
export function decodeFeedMarks(raw: unknown): FeedMarks {
  if (typeof raw !== "object" || raw === null) return emptyFeedMarks();
  const r = raw as Record<string, unknown>;
  if (r.version !== FEED_MARKS_VERSION) return emptyFeedMarks();
  const hidden = Array.isArray(r.hidden)
    ? [...new Set(r.hidden.filter((h): h is string => typeof h === "string" && HEX_RE.test(h)))].slice(
        0,
        MAX_HIDDEN,
      )
    : [];
  const looked: Record<string, string[]> = {};
  if (typeof r.looked === "object" && r.looked !== null) {
    for (const [k, v] of Object.entries(r.looked as Record<string, unknown>)) {
      if (!FEED_KEY_RE.test(k) || !Array.isArray(v)) continue;
      looked[k] = v.filter((x): x is string => typeof x === "string").slice(0, MAX_LOOKED_KEYS);
    }
  }
  const channelRead: Record<string, number> = {};
  if (typeof r.channelRead === "object" && r.channelRead !== null) {
    for (const [k, v] of Object.entries(r.channelRead as Record<string, unknown>)) {
      if (!CHANNEL_RE.test(k) || typeof v !== "number" || !Number.isSafeInteger(v) || v < 0) continue;
      channelRead[k] = v;
    }
  }
  return { hidden, looked, channelRead };
}

export function encodeFeedMarks(marks: FeedMarks): unknown {
  return { version: FEED_MARKS_VERSION, ...marks };
}

// ----- Hide from feeds -----------------------------------------------------

export function hideConversation(marks: FeedMarks, id: string): FeedMarks {
  if (marks.hidden.includes(id)) return marks;
  return { ...marks, hidden: [...marks.hidden, id].slice(-MAX_HIDDEN) };
}

export function showConversation(marks: FeedMarks, id: string): FeedMarks {
  if (!marks.hidden.includes(id)) return marks;
  return { ...marks, hidden: marks.hidden.filter((h) => h !== id) };
}

/** The sources a feed may draw from: every one not hidden. Applied before
 *  ranking, so a hidden conversation never reaches the core. */
export function withoutHidden<T extends { readonly idHex: string; readonly conversationHex?: string }>(
  sources: readonly T[],
  hidden: readonly string[],
): T[] {
  if (hidden.length === 0) return [...sources];
  const set = new Set(hidden);
  // Hidden by the CONVERSATION id (ADR-0073 R.5): a conversation hidden
  // before it moved to a successor group stays hidden after.
  return sources.filter((s) => !set.has(s.conversationHex ?? s.idHex));
}

/** Suggestions with hidden channels left out, before they are ranked. */
export function cardsWithoutHidden<T extends { readonly channelIdHex: string }>(
  cards: readonly T[],
  hidden: ReadonlySet<string>,
): T[] {
  if (hidden.size === 0) return [...cards];
  return cards.filter((c) => !hidden.has(c.channelIdHex));
}

// ----- "New" divider and "Show N new" ----------------------------------------

/** Remember what a feed showed when the person left it. */
export function recordLooked(marks: FeedMarks, feedKey: string, itemKeys: readonly string[]): FeedMarks {
  if (!FEED_KEY_RE.test(feedKey)) return marks;
  return { ...marks, looked: { ...marks.looked, [feedKey]: itemKeys.slice(0, MAX_LOOKED_KEYS) } };
}

/**
 * Where the "New" divider goes: the index of the first item that was not in
 * this feed the last time, or `null` when there is no divider — the first
 * visit (`previous` undefined) or nothing new. An item that was in the feed
 * last time is never "new", wherever it now sits.
 */
export function dividerIndex(
  itemKeys: readonly string[],
  previous: readonly string[] | undefined,
): number | null {
  if (previous === undefined) return null;
  const before = new Set(previous);
  const i = itemKeys.findIndex((k) => !before.has(k));
  return i < 0 ? null : i;
}

/** "Show N new": items in the waiting result that the drawn list lacks. */
export function pendingNewCount(shown: readonly string[], pending: readonly string[] | null): number {
  if (pending === null) return 0;
  const drawn = new Set(shown);
  let n = 0;
  for (const k of pending) if (!drawn.has(k)) n += 1;
  return n;
}

// ----- Channel read state ----------------------------------------------------

/** The newest post read in a channel moves forward only. */
export function advanceChannelRead(marks: FeedMarks, channelIdHex: string, cursor: number): FeedMarks {
  if (!CHANNEL_RE.test(channelIdHex) || !Number.isSafeInteger(cursor) || cursor <= 0) return marks;
  const current = marks.channelRead[channelIdHex] ?? 0;
  if (cursor <= current) return marks;
  return { ...marks, channelRead: { ...marks.channelRead, [channelIdHex]: cursor } };
}

/**
 * Give every channel that has none a newest-read marker at the newest post
 * this device holds of it right now (its slot's cursor, never below its
 * newest post), so a fresh subscription starts with no new posts and only
 * what arrives afterwards counts. A channel already marked is left alone.
 * Returns `marks` itself when nothing was added, so the caller writes
 * nothing.
 */
export function seedChannelReads(
  marks: FeedMarks,
  slots: { readonly [channelIdHex: string]: { readonly cursor: number; readonly posts: ReadonlyArray<{ readonly cursor: number }> } },
): FeedMarks {
  let added: Record<string, number> | null = null;
  for (const [hex, slot] of Object.entries(slots)) {
    if (!CHANNEL_RE.test(hex) || marks.channelRead[hex] !== undefined) continue;
    const newest = Math.max(slot.cursor, ...slot.posts.map((p) => p.cursor), 0);
    if (!Number.isSafeInteger(newest)) continue;
    added = { ...(added ?? {}), [hex]: newest };
  }
  return added === null ? marks : { ...marks, channelRead: { ...marks.channelRead, ...added } };
}

/**
 * A channel's count of new posts: the posts newer than the newest one read.
 * The person's own posts are read, so a channel this device posts to counts
 * none. Reading the newest post makes it zero, whatever lies below. A
 * channel not yet marked (`seedChannelReads` has not run for it) counts
 * none: what it holds then is what it held at subscription.
 */
export function channelUnreadCount(
  postCursors: readonly number[],
  newestRead: number | undefined,
  own: boolean,
): number {
  if (own || newestRead === undefined) return 0;
  const floor = newestRead;
  let n = 0;
  for (const c of postCursors) if (c > floor) n += 1;
  return n;
}

/** The badge text a count draws, as the chat list does. */
export function badgeText(count: number): string {
  return count >= 100 ? t("chats.row.unread_overflow") : count.toLocaleString(currentLanguage());
}
