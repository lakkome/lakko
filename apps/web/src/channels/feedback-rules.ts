// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The pure rules behind channel reactions and the ban list (ADR-0087 section
 * 13a), with no state and no network, so they can be tested on their own.
 * `post-feedback.ts` and `ban-list.ts` hold the state and re-export these.
 */
import type { Wasm } from "../wasm";

/** At most this many posts in one counts call (the service's cap). */
export const COUNTS_BATCH_MAX = 50;

function bytesOfHex(hex: string): Uint8Array {
  const out = new Uint8Array(Math.floor(hex.length / 2));
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** What a person's press of `emoji` on a post does, from their own reactions
 *  and the cap (reactions-and-emoji 10). Pure. */
export function channelTapVerdict(
  emoji: string,
  own: readonly string[],
  max: number,
  allowed: boolean,
): "add" | "remove" | "cap-reached" | "not-allowed" {
  if (own.includes(emoji)) return "remove";
  if (!allowed) return "not-allowed";
  if (own.length >= max) return "cap-reached";
  return "add";
}

/** The counts drawn: the service's, with this person's writes not yet seen
 *  in a fetch added or taken off, and never below the person's own reaction
 *  (their chip shows at once). Pure. */
export function drawnCounts(
  resolved: ReadonlyMap<string, number>,
  own: readonly string[],
  overlay: ReadonlyMap<string, number>,
): Map<string, number> {
  const out = new Map(resolved);
  for (const [emoji, delta] of overlay) {
    const next = (out.get(emoji) ?? 0) + delta;
    if (next > 0) out.set(emoji, next);
    else out.delete(emoji);
  }
  for (const emoji of own) if ((out.get(emoji) ?? 0) < 1) out.set(emoji, 1);
  return out;
}

/** Split `cursors` into the batches one call takes. Pure. */
export function countBatches(cursors: readonly number[], max = COUNTS_BATCH_MAX): number[][] {
  const unique = [...new Set(cursors.filter((c) => Number.isSafeInteger(c) && c > 0))];
  const out: number[][] = [];
  for (let i = 0; i < unique.length; i += max) out.push(unique.slice(i, i + max));
  return out;
}

/** Resolve one post's label counts through the core: emoji -> count.
 *  Labels no candidate produces are dropped. */
export function resolveLabelCounts(
  w: Wasm,
  seed: Uint8Array,
  channelId: Uint8Array,
  epoch: number,
  cursor: number,
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
  const key = w.ChannelReactionKey.derive(seed, channelId, epoch);
  try {
    const list = [...candidates];
    const emoji = key.resolveCountEmoji(BigInt(cursor), list, flat.subarray(0, i * 16), counts.subarray(0, i));
    const values = key.resolveCountValues(BigInt(cursor), list, flat.subarray(0, i * 16), counts.subarray(0, i));
    emoji.forEach((e, j) => {
      const v = Number(values[j] ?? 0n);
      if (v > 0) out.set(e, v);
    });
  } finally {
    key.free();
  }
  return out;
}


/** One folded ban list: its stamp and the banned author keys (lower-case
 *  hex, oldest first). */
export interface FoldedBanList {
  readonly updatedAtMs: bigint;
  readonly authors: readonly string[];
}

/** The newer of two lists by the core's rule (strictly newer wins; equal is
 *  not newer, so a re-issue with the original stamp changes nothing). Pure. */
export function foldBanList(
  stored: FoldedBanList | undefined,
  incoming: FoldedBanList,
  isNewer: (incomingMs: bigint, storedMs: bigint) => boolean,
): FoldedBanList {
  if (stored === undefined) return incoming;
  return isNewer(incoming.updatedAtMs, stored.updatedAtMs) ? incoming : stored;
}

/** The list after banning `authorHex`: appended at the end, the oldest dropped
 *  once over `max` (ADR-0087 13a: oldest go first). Pure. */
export function withBanned(authors: readonly string[], authorHex: string, max: number): string[] {
  const key = authorHex.toLowerCase();
  if (authors.includes(key)) return [...authors];
  const next = [...authors, key];
  return next.length > max ? next.slice(next.length - max) : next;
}

/** The list after unbanning `authorHex`. Pure. */
export function withoutBanned(authors: readonly string[], authorHex: string): string[] {
  const key = authorHex.toLowerCase();
  return authors.filter((a) => a !== key);
}

/** A stamp strictly newer than `stored`, whatever this clock says. Pure. */
export function nextBanStamp(stored: bigint | undefined, nowMs: bigint): bigint {
  return stored !== undefined && stored >= nowMs ? stored + 1n : nowMs;
}

/** Split a flat run of 32-byte keys into lower-case hex. Pure. */
export function splitAuthorKeys(flat: Uint8Array): string[] {
  const out: string[] = [];
  for (let i = 0; i + 32 <= flat.length; i += 32) {
    let hex = "";
    for (const b of flat.subarray(i, i + 32)) hex += b.toString(16).padStart(2, "0");
    out.push(hex);
  }
  return out;
}

/** The inverse of {@link splitAuthorKeys}. Pure. */
export function joinAuthorKeys(authors: readonly string[]): Uint8Array {
  const out = new Uint8Array(authors.length * 32);
  authors.forEach((hex, i) => {
    for (let j = 0; j < 32; j += 1) out[i * 32 + j] = Number.parseInt(hex.slice(j * 2, j * 2 + 2), 16);
  });
  return out;
}


// ---- what is kept on disk (sealed per account, `storage/account-kv.ts`) --

/** The persisted ban lists: the stamp as a decimal string (a `bigint` does
 *  not survive JSON), the names the banned authors' comments carried. */
export interface PersistedBanLists {
  readonly lists: readonly { readonly channel: string; readonly updatedAtMs: string; readonly authors: readonly string[] }[];
  readonly names: Readonly<Record<string, string>>;
}

const CHANNEL_HEX = /^[0-9a-f]{32}$/;
const KEY_HEX = /^[0-9a-f]{64}$/;

/** The JSON a set of folded lists is kept as. Pure. */
export function encodePersistedBanLists(
  lists: ReadonlyMap<string, FoldedBanList>,
  names: ReadonlyMap<string, string>,
): PersistedBanLists {
  const banned = new Set<string>();
  const out: { channel: string; updatedAtMs: string; authors: string[] }[] = [];
  for (const [channel, list] of lists) {
    out.push({ channel, updatedAtMs: list.updatedAtMs.toString(), authors: [...list.authors] });
    for (const a of list.authors) banned.add(a);
  }
  const keptNames: Record<string, string> = {};
  for (const [k, v] of names) if (banned.has(k)) keptNames[k] = v;
  return { lists: out, names: keptNames };
}

/** Read back what {@link encodePersistedBanLists} wrote; anything malformed
 *  is left out, never thrown. Pure. */
export function decodePersistedBanLists(raw: unknown): {
  readonly lists: Map<string, FoldedBanList>;
  readonly names: Map<string, string>;
} {
  const lists = new Map<string, FoldedBanList>();
  const names = new Map<string, string>();
  if (typeof raw !== "object" || raw === null) return { lists, names };
  const r = raw as { lists?: unknown; names?: unknown };
  if (Array.isArray(r.lists)) {
    for (const e of r.lists) {
      if (typeof e !== "object" || e === null) continue;
      const x = e as { channel?: unknown; updatedAtMs?: unknown; authors?: unknown };
      if (typeof x.channel !== "string" || !CHANNEL_HEX.test(x.channel)) continue;
      if (typeof x.updatedAtMs !== "string" || !/^\d{1,20}$/.test(x.updatedAtMs)) continue;
      if (!Array.isArray(x.authors)) continue;
      const authors = x.authors.filter((a): a is string => typeof a === "string" && KEY_HEX.test(a));
      lists.set(x.channel, { updatedAtMs: BigInt(x.updatedAtMs), authors });
    }
  }
  if (typeof r.names === "object" && r.names !== null) {
    for (const [k, v] of Object.entries(r.names as Record<string, unknown>)) {
      if (KEY_HEX.test(k) && typeof v === "string" && v.length <= 256) names.set(k, v);
    }
  }
  return { lists, names };
}

/** One reaction waiting for a token to leave. */
export interface WaitingReactionRecord {
  readonly idHex: string;
  readonly cursor: number;
  readonly epoch: number;
  readonly emoji: string;
}

/** Read back the persisted waiting reactions; malformed entries and repeats
 *  are left out. Pure. */
export function decodePersistedWaiting(raw: unknown): WaitingReactionRecord[] {
  if (!Array.isArray(raw)) return [];
  const out: WaitingReactionRecord[] = [];
  const seen = new Set<string>();
  for (const e of raw) {
    if (typeof e !== "object" || e === null) continue;
    const x = e as { idHex?: unknown; cursor?: unknown; epoch?: unknown; emoji?: unknown };
    if (typeof x.idHex !== "string" || !CHANNEL_HEX.test(x.idHex)) continue;
    if (typeof x.cursor !== "number" || !Number.isSafeInteger(x.cursor) || x.cursor <= 0) continue;
    if (typeof x.epoch !== "number" || !Number.isSafeInteger(x.epoch) || x.epoch < 0) continue;
    if (typeof x.emoji !== "string" || x.emoji.length === 0 || x.emoji.length > 64) continue;
    const key = `${x.idHex}:${x.cursor}:${x.emoji}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({ idHex: x.idHex, cursor: x.cursor, epoch: x.epoch, emoji: x.emoji });
  }
  return out;
}
