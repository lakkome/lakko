// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The channel-post log (ADR-0058 section 3): the posts of subscribed and
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

/** Payload schema version this client writes. */
export const CHANNEL_POST_LOG_VERSION = 1;
/** Most posts kept per channel (chosen, ADR-0058 open question 1). */
export const CHANNEL_POST_CAP = 500;
/** The server's post retention; older posts are dropped from the log. */
export const CHANNEL_POST_RETENTION_MS = 30 * 86_400_000;
/** Posts asked for per request. */
export const CHANNEL_POST_PAGE = 100;
/** Pages fetched per channel per sync, so one busy channel cannot hold the
 *  loop; the rest arrives on the next sync. */
export const CHANNEL_POST_MAX_PAGES = 5;

/** One decrypted text post. */
export interface PersistedChannelPost {
  /** The server's per-channel cursor, the post's id in the feed. */
  readonly cursor: number;
  /** Server receive time, unix ms. */
  readonly serverTsMs: number;
  readonly text: string;
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
  return { cursor: r.cursor, serverTsMs: r.serverTsMs, text: r.text };
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
): ChannelPostSlot {
  const byCursor = new Map<number, PersistedChannelPost>();
  for (const p of slot?.posts ?? []) byCursor.set(p.cursor, p);
  for (const p of fresh) byCursor.set(p.cursor, p);
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
  /** A text post: kept. */
  | { readonly kind: "text"; readonly text: string }
  /** Read and not a feed item (a metadata record, a malformed record, a
   *  post that failed to open under its epoch's seed): passed over. */
  | { readonly kind: "skip" }
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
 * post, keep the text ones. Stops at the first post that must wait for a
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
      if (outcome.kind === "text") {
        fresh.push({ cursor: p.cursor, serverTsMs: p.serverTsMs, text: outcome.text });
      }
      cursor = p.cursor;
    }
    if (blocked || res.posts.length < CHANNEL_POST_PAGE) break;
  }
  return mergeChannelPosts(slot, fresh, cursor, nowMs);
}
