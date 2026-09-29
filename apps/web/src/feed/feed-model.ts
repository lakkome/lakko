// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The feed's input, assembled from what this device already holds
 * (ADR-0058 section 1, `docs/ux/feed.md` section 9).
 *
 * The ranking is not here: it is `lakko-feed` in Rust, reached through
 * `feed-ranking.ts`, so the three clients order the same messages the same
 * way. This module only turns the per-account message log into the flat
 * conversation and candidate lists the ranking reads, and keeps a reference
 * from each candidate back to the row it came from so the screen can draw
 * it. It is plain data in, plain data out, and has no Solid, no wasm and
 * no storage in it, so it is tested directly.
 *
 * One-to-one chats never reach it: the caller passes groups and channels
 * only, and the ranking's conversation kind has no value for a one-to-one
 * (UX spec section 3).
 */

import type { PersistedGroup, PersistedMessage } from "../chat/message-log-store";
import type { ChannelPostSlot } from "./channel-post-log";

/** One group or channel the feed may take items from. */
export interface FeedSource {
  readonly kind: "group" | "channel";
  /** The log key: the message-log key of a group, the 32-char hex id of a
   *  channel. */
  readonly idHex: string;
  /** The route segment that opens it: base64url of a group id
   *  (`/groups/<slug>`), the hex id of a channel (`/channels/<id>`). */
  readonly slug: string;
  /** What the conversation list calls it. */
  readonly title: string;
  /** Muted in this account's conversation flags. */
  readonly muted: boolean;
  /** People in a group, for weighing reactions against its size; 0 for a
   *  channel, whose posts carry no reactions. */
  readonly audience: number;
}

/** One candidate in the shape `rankFeed` takes, before it crosses to wasm. */
export interface FeedCandidateInput {
  /** Index into the conversation list. */
  readonly conversation: number;
  readonly messageId: bigint;
  readonly replyTo: bigint | null;
  readonly timeMs: bigint;
  readonly fromSelf: boolean;
  readonly fromContact: boolean;
  readonly mentionsMe: boolean;
  readonly repliesToMe: boolean;
  readonly reactionsDistinct: number;
  readonly hasMedia: boolean;
  readonly text: string;
}

/** Where a candidate came from, so the screen can draw and open it. */
export interface FeedCandidateRef {
  readonly source: FeedSource;
  readonly message: PersistedMessage;
  /** For the item's "why" line (UX spec section 3). */
  readonly mentionsMe: boolean;
  readonly repliesToMe: boolean;
}

/** What `buildFeedInput` returns: parallel lists, one ref per candidate. */
export interface FeedInput {
  readonly sources: readonly FeedSource[];
  readonly candidates: readonly FeedCandidateInput[];
  readonly refs: readonly FeedCandidateRef[];
}

/** Everything `buildFeedInput` reads besides the log. */
export interface FeedContext {
  readonly sources: readonly FeedSource[];
  /** The message log, for groups. */
  readonly log: { readonly [idHex: string]: PersistedGroup };
  /** The channel-post log, for channels. */
  readonly channelPosts: { readonly [idHex: string]: ChannelPostSlot };
  /** Channels this device publishes to: their posts are the person's own. */
  readonly ownChannels: ReadonlySet<string>;
  /** The person's own username, for mention matching, when known. */
  readonly ownUsername: string | null;
  /** Signature keys (base64) of accepted contacts. */
  readonly contactKeysB64: ReadonlySet<string>;
  /** The mention rule, `mentionsUsername` from the core. */
  readonly mentions: (text: string, username: string) => boolean;
  /** Now, in unix milliseconds. */
  readonly nowMs: number;
}

/**
 * Oldest message the feed ever looks at: the longest window a rule can ask
 * for (`lakko-feed` `MAX_WINDOW_DAYS`, 30), which is also how long the
 * server keeps a message. Older rows are not handed to the ranking at all,
 * so the cost of a feed build tracks the last month, not the whole log.
 */
export const FEED_MAX_WINDOW_MS = 30 * 86_400_000;

function parseId(raw: string): bigint | null {
  if (!/^\d{1,19}$/.test(raw)) return null;
  const id = BigInt(raw);
  return id <= 0x7fff_ffff_ffff_ffffn ? id : null;
}

/**
 * A channel's posts as candidates. A post has no author but the channel, no
 * reply and no reactions; it is the person's own when this device publishes
 * to the channel. Its row is shaped as a message so the screen draws both
 * kinds of item one way.
 */
function addChannelPosts(
  ctx: FeedContext,
  source: FeedSource,
  conversation: number,
  oldest: number,
  candidates: FeedCandidateInput[],
  refs: FeedCandidateRef[],
): void {
  const slot = ctx.channelPosts[source.idHex];
  if (slot === undefined) return;
  const fromSelf = ctx.ownChannels.has(source.idHex);
  for (const p of slot.posts) {
    if (p.cursor <= 0 || p.serverTsMs < oldest) continue;
    const mentionsMe =
      !fromSelf && ctx.ownUsername !== null && ctx.mentions(p.text, ctx.ownUsername);
    candidates.push({
      conversation,
      messageId: BigInt(p.cursor),
      replyTo: null,
      timeMs: BigInt(Math.trunc(p.serverTsMs)),
      fromSelf,
      fromContact: false,
      mentionsMe,
      repliesToMe: false,
      reactionsDistinct: 0,
      hasMedia: false,
      text: p.text,
    });
    const message: PersistedMessage = {
      messageId: String(p.cursor),
      direction: fromSelf ? "outgoing" : "incoming",
      text: p.text,
      attachments: [],
      createdAt: new Date(p.serverTsMs).toISOString(),
    };
    refs.push({ source, message, mentionsMe, repliesToMe: false });
  }
}

/** Turn the logs into the ranking's input. Rows that cannot be placed (a
 *  system row, a deletion, an unparseable id or time) are left out. */
export function buildFeedInput(ctx: FeedContext): FeedInput {
  const oldest = ctx.nowMs - FEED_MAX_WINDOW_MS;
  const candidates: FeedCandidateInput[] = [];
  const refs: FeedCandidateRef[] = [];

  ctx.sources.forEach((source, conversation) => {
    if (source.kind === "channel") {
      addChannelPosts(ctx, source, conversation, oldest, candidates, refs);
      return;
    }
    const slot = ctx.log[source.idHex];
    if (slot === undefined) return;
    const outgoing = new Set<string>();
    for (const m of slot.messages) {
      if (m.direction === "outgoing") outgoing.add(m.messageId);
    }
    for (const m of slot.messages) {
      if (m.direction === "system" || m.deleted === true) continue;
      const id = parseId(m.messageId);
      if (id === null) continue;
      const time = Date.parse(m.createdAt);
      if (Number.isNaN(time) || time < oldest) continue;
      const replyTarget = m.reply?.target ?? null;
      const replyTo = replyTarget === null ? null : parseId(replyTarget);
      const fromSelf = m.direction === "outgoing";
      const author = m.authorKeyB64 ?? null;
      const reactors = new Set((m.reactions ?? []).map((r) => r.reactorKeyB64));
      const mentionsMe =
        !fromSelf && ctx.ownUsername !== null && ctx.mentions(m.text, ctx.ownUsername);
      const repliesToMe = !fromSelf && replyTarget !== null && outgoing.has(replyTarget);
      candidates.push({
        conversation,
        messageId: id,
        replyTo,
        timeMs: BigInt(time),
        fromSelf,
        fromContact: !fromSelf && author !== null && ctx.contactKeysB64.has(author),
        mentionsMe,
        repliesToMe,
        reactionsDistinct: reactors.size,
        hasMedia: m.attachments.length > 0,
        text: m.text,
      });
      refs.push({ source, message: m, mentionsMe, repliesToMe });
    }
  });

  return { sources: ctx.sources, candidates, refs };
}

/** The top reactions of a message for its summary: emoji with counts,
 *  most first, at most `limit`. Ties keep first-seen order. */
export function reactionSummary(
  message: PersistedMessage,
  limit = 3,
): ReadonlyArray<{ readonly emoji: string; readonly count: number }> {
  const counts = new Map<string, number>();
  for (const r of message.reactions ?? []) {
    counts.set(r.emoji, (counts.get(r.emoji) ?? 0) + 1);
  }
  return Array.from(counts, ([emoji, count]) => ({ emoji, count }))
    .sort((a, b) => b.count - a.count)
    .slice(0, limit);
}
