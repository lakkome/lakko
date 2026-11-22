// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The channel-post log (ADR-0063 section 3): the posts of subscribed and
 * own channels this device has decrypted, kept so the feed can rank them
 * without opening each channel.
 *
 * Pure: the codec, the merge and the per-channel sync live here with their
 * network and wasm dependencies injected, so `vitest` exercises them with
 * no IndexedDB, no fetch and no wasm. The store (`channel-post-log-store.ts`)
 * seals this payload under the device root key; the runner
 * (`ChannelPostSync.tsx`) drives the sync while the app is in front.
 *
 * Bounds, both chosen: posts older than the server's 30-day retention are
 * dropped (the log never keeps what the server could no longer serve), and
 * at most `CHANNEL_POST_CAP` posts are kept per channel, newest kept.
 */

import type { PersistedAttachment } from "../chat/message-log-store";

/** Payload schema version this client writes. Version 2 records each
 *  post's attachment kinds; a version-1 log (attachment posts stored as
 *  empty text) is dropped and fetched again, so a caption-less photo already
 *  held is named on the row too. Version 3 also keeps each attachment's
 *  reference, so the feed draws a channel's photo, GIF, voice or video
 *  message in place as it draws a group's (`docs/ux/feed.md` section 3, base
 *  scenario 32); a version-2 log is dropped and fetched again the same way
 *  (read markers live apart, in `feed-marks.ts`, and survive). Version 4
 *  also keeps a content post's content (ADR-0092: a poll, checklist,
 *  location, contact card or article), dropped and fetched again the same
 *  way. */
export const CHANNEL_POST_LOG_VERSION = 4;
/** Most posts kept per channel (chosen, ADR-0063 open question 1). */
export const CHANNEL_POST_CAP = 500;
/** The server's post retention; older posts are dropped from the log. */
export const CHANNEL_POST_RETENTION_MS = 30 * 86_400_000;
/** Posts asked for per request. */
export const CHANNEL_POST_PAGE = 100;
/** Pages fetched per channel per sync, so one busy channel cannot hold the
 *  loop; the rest arrives on the next sync. */
export const CHANNEL_POST_MAX_PAGES = 5;

/** What the log keeps of one attachment: what names its kind on a list
 *  row ("Photo", "GIF", "File"), and the reference the feed downloads and
 *  opens it by. The log is sealed under the device key
 *  (`channel-post-log-store.ts`), as the group message log holding the same
 *  references is. */
export interface PersistedChannelPostMedia {
  readonly mime: string | null;
  readonly kind: string | null;
  /** Absent when it was not kept (a test row) or did not decode. */
  readonly ref?: PersistedAttachment;
}

/** One decrypted post. */
export interface PersistedChannelPost {
  /** The server's per-channel cursor, the post's id in the feed. */
  readonly cursor: number;
  /** Server receive time, unix ms. */
  readonly serverTsMs: number;
  /** The post's text; empty for a photo, file or GIF posted without one. */
  readonly text: string;
  /** The post's attachments, in order; absent when it has none. A channel's
   *  row in Chats names the kind of the first when the text is empty, as a
   *  group's row does. */
  readonly media?: readonly PersistedChannelPostMedia[];
  /** ADR-0092 — a content post's `MessageContent`, verbatim; absent on an
   *  ordinary post. Its images (an article's) are in `media`. */
  readonly content?: unknown;
}

/** Most attachments recorded per post: the row names the first and counts
 *  the rest, and a post carries few (chosen). */
export const CHANNEL_POST_MEDIA_CAP = 16;

/** A stored attachment reference: the four fields it cannot be fetched
 *  without are strings, else it is left out (the media entry survives with
 *  its kind). The optional metadata is re-checked on use by
 *  `attachmentRefFromPersisted`. */
function decodeRef(raw: unknown): PersistedAttachment | undefined {
  if (typeof raw !== "object" || raw === null) return undefined;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.blobIdB64 !== "string" ||
    typeof r.keyB64 !== "string" ||
    typeof r.nonceB64 !== "string" ||
    typeof r.sizeStr !== "string" ||
    !/^[0-9]+$/.test(r.sizeStr)
  ) {
    return undefined;
  }
  return raw as PersistedAttachment;
}

function decodeMedia(raw: unknown): PersistedChannelPostMedia | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  const mime = typeof r.mime === "string" ? r.mime : null;
  const kind = typeof r.kind === "string" ? r.kind : null;
  const ref = decodeRef(r.ref);
  return { mime, kind, ...(ref === undefined ? {} : { ref }) };
}

/** One channel's slice of the log. */
export interface ChannelPostSlot {
  /** Every post at or below this cursor has been looked at. */
  readonly cursor: number;
  /** Ascending by cursor. */
  readonly posts: readonly PersistedChannelPost[];
}

export interface ChannelPostLog {
  readonly version: number;
  readonly channels: { readonly [channelIdHex: string]: ChannelPostSlot };
}

export function emptyChannelPostLog(): ChannelPostLog {
  return { version: CHANNEL_POST_LOG_VERSION, channels: {} };
}

const CHANNEL_ID_RE = /^[0-9a-f]{32}$/;

function isCursor(n: unknown): n is number {
  return typeof n === "number" && Number.isSafeInteger(n) && n >= 0;
}

function decodePost(raw: unknown): PersistedChannelPost | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  if (!isCursor(r.cursor) || r.cursor === 0) return null;
  if (typeof r.serverTsMs !== "number" || !Number.isFinite(r.serverTsMs)) return null;
  if (typeof r.text !== "string") return null;
  const media = Array.isArray(r.media)
    ? r.media
        .slice(0, CHANNEL_POST_MEDIA_CAP)
        .map(decodeMedia)
        .filter((m): m is PersistedChannelPostMedia => m !== null)
    : [];
  return {
    cursor: r.cursor,
    serverTsMs: r.serverTsMs,
    text: r.text,
    ...(media.length > 0 ? { media } : {}),
    ...(r.content === undefined || r.content === null ? {} : { content: r.content }),
  };
}

/**
 * Decode a stored payload. An unknown version is `null` (the caller starts
 * empty); a malformed channel or post is dropped alone.
 */
export function decodeChannelPostLog(raw: unknown): ChannelPostLog | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  if (r.version !== CHANNEL_POST_LOG_VERSION) return null;
  const channels: Record<string, ChannelPostSlot> = {};
  if (typeof r.channels === "object" && r.channels !== null) {
    for (const [id, slot] of Object.entries(r.channels as Record<string, unknown>)) {
      if (!CHANNEL_ID_RE.test(id) || typeof slot !== "object" || slot === null) continue;
      const s = slot as Record<string, unknown>;
      if (!isCursor(s.cursor)) continue;
      const posts = Array.isArray(s.posts)
        ? s.posts.map(decodePost).filter((p): p is PersistedChannelPost => p !== null)
        : [];
      channels[id] = { cursor: s.cursor, posts: posts.sort((a, b) => a.cursor - b.cursor) };
    }
  }
  return { version: CHANNEL_POST_LOG_VERSION, channels };
}

/**
 * Fold newly decrypted posts into a slot: de-duplicated by cursor (a later
 * copy wins), ascending, trimmed to the retention window and the cap. The
 * cursor never moves backwards.
 */
export function mergeChannelPosts(
  slot: ChannelPostSlot | undefined,
  fresh: readonly PersistedChannelPost[],
  cursor: number,
  nowMs: number,
  /** `docs/ux/message-actions.md` section 9 — the posts a delete op removed:
   *  they leave the log, so they are never a channel's preview, never unread
   *  and never in the feed. */
  deleted: ReadonlySet<number> = new Set(),
): ChannelPostSlot {
  const byCursor = new Map<number, PersistedChannelPost>();
  for (const p of slot?.posts ?? []) byCursor.set(p.cursor, p);
  for (const p of fresh) byCursor.set(p.cursor, p);
  for (const c of deleted) byCursor.delete(c);
  const oldest = nowMs - CHANNEL_POST_RETENTION_MS;
  const posts = Array.from(byCursor.values())
    .filter((p) => p.serverTsMs >= oldest)
    .sort((a, b) => a.cursor - b.cursor)
    .slice(-CHANNEL_POST_CAP);
  return { cursor: Math.max(slot?.cursor ?? 0, cursor), posts };
}

/** Drop the slots of channels this device no longer holds. */
export function retainChannels(log: ChannelPostLog, keep: ReadonlySet<string>): ChannelPostLog {
  const channels: Record<string, ChannelPostSlot> = {};
  for (const [id, slot] of Object.entries(log.channels)) {
    if (keep.has(id)) channels[id] = slot;
  }
  return { version: log.version, channels };
}

// ----- Sync -------------------------------------------------------------

/** What one post decoded to, as far as the log cares. */
export type PostOutcome =
  /** A post (text, attachments or both): kept. */
  | {
      readonly kind: "text";
      readonly text: string;
      readonly media?: readonly PersistedChannelPostMedia[];
      readonly content?: unknown;
    }
  /** Read and not a feed item (a metadata record, a malformed record, a
   *  post that failed to open under its epoch's seed): passed over. */
  | { readonly kind: "skip" }
  /** A delete op of an earlier post (`message-actions.md` section 9): the
   *  post leaves the log. */
  | { readonly kind: "delete"; readonly target: number }
  /** No seed for the post's epoch and one may still arrive (the epoch is
   *  newer than every seed this device holds): the cursor stops before it
   *  so the next sync tries again. */
  | { readonly kind: "wait" };

export interface ChannelPostPage {
  readonly posts: ReadonlyArray<{
    readonly cursor: number;
    readonly serverTsMs: number;
    readonly payloadB64: string;
  }>;
  readonly nextAfter: number;
}

export interface ChannelSyncDeps {
  readonly fetchPage: (channelIdHex: string, after: number, limit: number) => Promise<ChannelPostPage>;
  readonly decode: (channelIdHex: string, payloadB64: string) => Promise<PostOutcome>;
}

/**
 * Bring one channel's slot up to date: fetch after its cursor, decode each
 * post, keep the posts (text, attachments or both). Stops at the first post that must wait for a
 * seed, at an empty page, or after `CHANNEL_POST_MAX_PAGES` pages. A fetch
 * failure throws; the slot the caller holds is unchanged.
 */
export async function syncChannelSlot(
  channelIdHex: string,
  slot: ChannelPostSlot | undefined,
  deps: ChannelSyncDeps,
  nowMs: number,
): Promise<ChannelPostSlot> {
  let cursor = slot?.cursor ?? 0;
  const fresh: PersistedChannelPost[] = [];
  const deleted = new Set<number>();
  for (let page = 0; page < CHANNEL_POST_MAX_PAGES; page += 1) {
    const res = await deps.fetchPage(channelIdHex, cursor, CHANNEL_POST_PAGE);
    if (res.posts.length === 0) break;
    let blocked = false;
    for (const p of [...res.posts].sort((a, b) => a.cursor - b.cursor)) {
      if (p.cursor <= cursor) continue;
      const outcome = await deps.decode(channelIdHex, p.payloadB64);
      if (outcome.kind === "wait") {
        blocked = true;
        break;
      }
      if (outcome.kind === "delete") {
        // An op only reaches back.
        if (outcome.target < p.cursor) deleted.add(outcome.target);
      }
      if (outcome.kind === "text") {
        const media = (outcome.media ?? []).slice(0, CHANNEL_POST_MEDIA_CAP);
        fresh.push({
          cursor: p.cursor,
          serverTsMs: p.serverTsMs,
          text: outcome.text,
          ...(media.length > 0 ? { media } : {}),
          ...(outcome.content === undefined ? {} : { content: outcome.content }),
        });
      }
      cursor = p.cursor;
    }
    if (blocked || res.posts.length < CHANNEL_POST_PAGE) break;
  }
  return mergeChannelPosts(slot, fresh, cursor, nowMs, deleted);
}
