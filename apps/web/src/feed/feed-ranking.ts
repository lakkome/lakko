// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The one ranking, reached through the wasm core (ADR-0063).
 *
 * The order lives in `lakko-feed` so web, iOS and Android draw the same
 * messages in the same order; this module only moves `buildFeedInput`'s
 * lists across the boundary and maps the returned indices back to rows. It
 * re-derives nothing: a filter or a tie-break written here would be the
 * drift the core exists to prevent.
 */

import type { Wasm } from "../wasm";
import { feedSourceKey, type FeedCandidateRef, type FeedInput } from "./feed-model";

/** The slice of the wasm module the feed uses. */
export type FeedCore = Pick<
  Wasm,
  | "FeedRule"
  | "FeedPreset"
  | "FeedConversation"
  | "FeedConversationKind"
  | "FeedCandidate"
  | "rankFeed"
  | "mentionsUsername"
  | "contiguousSeen"
>;

/** One item to draw: a message and the replies folded under it. */
export interface RankedItem {
  readonly ref: FeedCandidateRef;
  readonly replies: readonly FeedCandidateRef[];
}

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length >> 1);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

/**
 * Rank `input` under `rule` at `nowMs`.
 *
 * The wasm objects built for the call are consumed by it; the entries it
 * returns are read and freed here, so nothing crosses back but indices.
 */
export function rankFeedInput(
  core: FeedCore,
  rule: InstanceType<FeedCore["FeedRule"]>,
  input: FeedInput,
  nowMs: number,
): RankedItem[] {
  const conversations = input.sources.map(
    (s) =>
      new core.FeedConversation(
        // The conversation id: what a feed rule's lists name (ADR-0073 R.5).
        hexToBytes(feedSourceKey(s)),
        s.kind === "channel" ? core.FeedConversationKind.Channel : core.FeedConversationKind.Group,
        s.muted,
        Math.max(0, Math.min(s.audience, 0xffff_ffff)),
      ),
  );
  const candidates = input.candidates.map(
    (c) =>
      new core.FeedCandidate(
        c.conversation,
        c.messageId,
        c.replyTo ?? undefined,
        c.timeMs,
        c.fromSelf,
        c.fromContact,
        c.mentionsMe,
        c.repliesToMe,
        c.reactionsDistinct,
        c.hasMedia,
        c.text,
      ),
  );
  const entries = core.rankFeed(rule, conversations, candidates, BigInt(Math.trunc(nowMs)));
  const out: RankedItem[] = [];
  for (const e of entries) {
    const ref = input.refs[e.index];
    const replies = Array.from(e.replies, (i) => input.refs[i]).filter(
      (r): r is FeedCandidateRef => r !== undefined,
    );
    e.free();
    if (ref !== undefined) out.push({ ref, replies });
  }
  return out;
}
