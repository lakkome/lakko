// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The pure rules behind reactions on channel comments and the comment count
 * under a post (ADR-0087 section 13a, phase B), with no state and no
 * network, so they can be tested on their own. `comment-feedback.ts` holds
 * the state.
 */
import type { Wasm } from "../wasm";

const CHANNEL_HEX = /^[0-9a-f]{32}$/;

/** One comment, named. */
export interface CommentRef {
  readonly idHex: string;
  readonly post: number;
  readonly comment: number;
}

/** The key one comment's state is filed under. */
export function commentKey(r: CommentRef): string {
  return `${r.idHex.toLowerCase()}:${r.post}:${r.comment}`;
}

/** A reaction on a comment that waits for a token. */
export interface WaitingCommentReaction extends CommentRef {
  readonly epoch: number;
  readonly emoji: string;
  /** The post's time (the own register's retention runs from it). */
  readonly postAtMs: number;
}

function isPositiveInt(v: unknown): v is number {
  return typeof v === "number" && Number.isSafeInteger(v) && v > 0;
}

function isEmoji(v: unknown): v is string {
  return typeof v === "string" && v.length > 0 && v.length <= 64;
}

function parseRef(x: { idHex?: unknown; post?: unknown; comment?: unknown }): CommentRef | null {
  if (typeof x.idHex !== "string" || !CHANNEL_HEX.test(x.idHex)) return null;
  if (!isPositiveInt(x.post) || !isPositiveInt(x.comment)) return null;
  return { idHex: x.idHex, post: x.post, comment: x.comment };
}

/** Read back the persisted waiting comment reactions; malformed entries and
 *  repeats are left out. Pure. */
export function decodeWaitingCommentReactions(raw: unknown): WaitingCommentReaction[] {
  if (!Array.isArray(raw)) return [];
  const out: WaitingCommentReaction[] = [];
  const seen = new Set<string>();
  for (const e of raw) {
    if (typeof e !== "object" || e === null) continue;
    const x = e as {
      idHex?: unknown;
      post?: unknown;
      comment?: unknown;
      epoch?: unknown;
      emoji?: unknown;
      postAtMs?: unknown;
    };
    const ref = parseRef(x);
    if (ref === null) continue;
    if (typeof x.epoch !== "number" || !Number.isSafeInteger(x.epoch) || x.epoch < 0) continue;
    if (!isEmoji(x.emoji)) continue;
    if (typeof x.postAtMs !== "number" || !Number.isSafeInteger(x.postAtMs) || x.postAtMs < 0) continue;
    const k = `${commentKey(ref)}:${x.emoji}`;
    if (seen.has(k)) continue;
    seen.add(k);
    out.push({ ...ref, epoch: x.epoch, emoji: x.emoji, postAtMs: x.postAtMs });
  }
  return out;
}

/** Split up to 50 comment cursors per call. Pure. */
export function commentCountBatches(cursors: readonly number[], max = 50): number[][] {
  const unique = [...new Set(cursors.filter((c) => Number.isSafeInteger(c) && c > 0))].sort((a, b) => a - b);
  const out: number[][] = [];
  for (let i = 0; i < unique.length; i += max) out.push(unique.slice(i, i + max));
  return out;
}

function bytesOfHex(hex: string): Uint8Array {
  const out = new Uint8Array(Math.floor(hex.length / 2));
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** Resolve one comment's label counts through the core: emoji -> count.
 *  Labels no candidate produces are dropped. */
export function resolveCommentLabelCounts(
  w: Wasm,
  seed: Uint8Array,
  channelId: Uint8Array,
  epoch: number,
  post: number,
  comment: number,
  labels: ReadonlyMap<string, number>,
  candidates: readonly string[],
): Map<string, number> {
  const out = new Map<string, number>();
  if (labels.size === 0 || candidates.length === 0) return out;
  const flat = new Uint8Array(labels.size * 16);
  const counts = new BigUint64Array(labels.size);
  let i = 0;
  for (const [hex, count] of labels) {
    if (hex.length !== 32) continue;
    flat.set(bytesOfHex(hex), i * 16);
    counts[i] = BigInt(Math.max(0, Math.floor(count)));
    i += 1;
  }
  const key = w.ChannelCommentReactionKey.derive(seed, channelId, epoch);
  try {
    const list = [...candidates];
    const l = flat.subarray(0, i * 16);
    const c = counts.subarray(0, i);
    const emoji = key.resolveCountEmoji(BigInt(post), BigInt(comment), list, l, c);
    const values = key.resolveCountValues(BigInt(post), BigInt(comment), list, l, c);
    emoji.forEach((e, j) => {
      const v = Number(values[j] ?? 0n);
      if (v > 0) out.set(e, v);
    });
  } finally {
    key.free();
  }
  return out;
}
