// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Reactions under channel posts (ADR-0087 section 13a; UX
 * `group-and-channel-privacy.md` 13.1, `reactions-and-emoji.md` 8.3, 10, 11).
 *
 * - **Counts.** The posts in view are asked in one batched call (at most 50
 *   a call). The service answers labels it cannot read; a label is resolved by
 *   trying every emoji this app knows under the post's reaction key (derived
 *   from the post's epoch seed). An unknown label is dropped. A failed call
 *   leaves the last counts and says nothing.
 * - **Own reactions.** Kept in the account's synced state
 *   (`own-feedback-register.ts`), never asked of the service. They drive the
 *   highlight and the client-side cap of 3 per post.
 * - **Writing.** Each reaction has a key of its own (`ChannelReactionWriteKey`
 *   over the account seed, channel, post and label), so the service cannot
 *   link two of one person's reactions. Adding spends a `channel-react` token;
 *   with no token to spend it waits and leaves later, with the chip already
 *   drawn and nothing said. The waiting ones are kept per account, sealed
 *   (`storage/account-kv.ts`, row `channel-reactions-waiting`), so they
 *   survive a restart. Taking one off spends none and goes at once.
 */
import { createSignal } from "solid-js";

import rawEmoji from "../media/emoji/emoji-data.json";
import { buildDataset, loadRecentReactions, type RawEmojiData } from "../media/emoji/emoji-index";
import { channelSeedFor } from "./channel-access";
import { addReactionRow, deleteReactionRow, fetchReactionCounts, FeedbackApiError, hexOf, bytesOfHex } from "./feedback-api";
import { ownFeedbackPort, ownReactionsOn } from "./own-feedback-register";
import { anonTokenQuotaExhausted, isAnonTokenNetworkFailure } from "../tokens/anon-token-pool";
import { loadWasm, wasmSync, type Wasm } from "../wasm";
import { loadAccountJson, saveAccountJson } from "../storage/account-kv";
import { bindCommentReactionAccount, forgetCommentReactions, kickWaitingCommentReactions } from "./comment-feedback";
import {
  decodePersistedWaiting,
  type WaitingReactionRecord,
  channelTapVerdict,
  countBatches,
  COUNTS_BATCH_MAX,
  drawnCounts,
  resolveLabelCounts,
} from "./feedback-rules";

export { channelTapVerdict, countBatches, COUNTS_BATCH_MAX, drawnCounts, resolveLabelCounts };

type Identity = InstanceType<Wasm["Identity"]>;

/** Between tries to send a reaction that is waiting for a token (chosen). */
const WAITING_RETRY_MS = 30_000;

// ---- candidates ---------------------------------------------------------

let knownEmoji: string[] | null = null;

/** Every emoji this app knows (the bundled set with its tone variants). */
function bundledEmoji(): string[] {
  if (knownEmoji === null) {
    try {
      knownEmoji = [...buildDataset(rawEmoji as unknown as RawEmojiData).byEmoji.keys()];
    } catch {
      knownEmoji = [];
    }
  }
  return knownEmoji;
}

/** The candidates a label is resolved against: the person's own and recent
 *  reactions first, then every emoji the app knows. */
export function reactionCandidates(own: readonly string[], recent: readonly string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const e of [...own, ...recent, ...bundledEmoji()]) {
    if (!seen.has(e)) {
      seen.add(e);
      out.push(e);
    }
  }
  return out;
}

// ---- counts store -------------------------------------------------------

const [rawCounts, setRawCounts] = createSignal<ReadonlyMap<string, ReadonlyMap<number, ReadonlyMap<string, number>>>>(
  new Map(),
);
/** `${channel}:${cursor}` -> the post's comment count as last fetched
 *  (ADR-0087 phase B: the service's maintained counter, never a count of
 *  rows this device read). */
const [commentCounts, setCommentCounts] = createSignal<ReadonlyMap<string, number>>(new Map());
/** `${channel}:${cursor}` -> a channel poll's voters and votes per opaque
 *  option label as last fetched (ADR-0092 section 4). */
const [voteCounts, setVoteCounts] = createSignal<
  ReadonlyMap<string, { readonly voters: number; readonly labels: ReadonlyMap<string, number> }>
>(new Map());
/** `${channel}:${cursor}` -> emoji -> delta of this person's writes the last
 *  counts do not show yet. */
const [overlays, setOverlays] = createSignal<ReadonlyMap<string, ReadonlyMap<string, number>>>(new Map());
/** Writes whose delta clears once a fetch issued after them answers:
 *  overlay key -> emoji -> the fetch number that must be passed. */
const settleAfter = new Map<string, Map<string, number>>();
let fetchIssue = 0;

const pending = new Map<string, Set<number>>();
let flushTimer: ReturnType<typeof setTimeout> | null = null;

function overlayKey(idHex: string, cursor: number): string {
  return `${idHex.toLowerCase()}:${cursor}`;
}

/** The raw label counts of one post as last fetched. Reactive. */
export function postLabelCounts(idHex: string, cursor: number): ReadonlyMap<string, number> | undefined {
  return rawCounts().get(idHex.toLowerCase())?.get(cursor);
}

/** The post's comment count as the service last answered, or `undefined`
 *  before it answered (or from a service that does not report it).
 *  Reactive. */
export function postCommentCount(idHex: string, cursor: number): number | undefined {
  return commentCounts().get(overlayKey(idHex, cursor));
}

/** A channel poll's vote counts as last fetched, or `undefined`. Reactive. */
export function postVoteCounts(
  idHex: string,
  cursor: number,
): { readonly voters: number; readonly labels: ReadonlyMap<string, number> } | undefined {
  return voteCounts().get(overlayKey(idHex, cursor));
}

/** This person's not-yet-counted writes on one post. Reactive. */
export function postOverlay(idHex: string, cursor: number): ReadonlyMap<string, number> {
  return overlays().get(overlayKey(idHex, cursor)) ?? new Map();
}

function bumpOverlay(idHex: string, cursor: number, emoji: string, delta: number): void {
  const k = overlayKey(idHex, cursor);
  setOverlays((m) => {
    const inner = new Map(m.get(k) ?? []);
    const next = (inner.get(emoji) ?? 0) + delta;
    if (next === 0) inner.delete(emoji);
    else inner.set(emoji, next);
    const out = new Map(m);
    if (inner.size === 0) out.delete(k);
    else out.set(k, inner);
    return out;
  });
}

function clearOverlay(idHex: string, cursor: number, emoji: string): void {
  const k = overlayKey(idHex, cursor);
  setOverlays((m) => {
    const inner = new Map(m.get(k) ?? []);
    if (!inner.has(emoji)) return m;
    inner.delete(emoji);
    const out = new Map(m);
    if (inner.size === 0) out.delete(k);
    else out.set(k, inner);
    return out;
  });
}

async function fetchBatch(idHex: string, cursors: number[]): Promise<void> {
  fetchIssue += 1;
  const issued = fetchIssue;
  let answer;
  try {
    answer = await fetchReactionCounts(idHex, cursors);
  } catch {
    // The last counts stay; nothing is said (reactions-and-emoji 11).
    return;
  }
  const byCursor = new Map(answer.map((p) => [p.cursor, p.labels] as const));
  const key = idHex.toLowerCase();
  setRawCounts((m) => {
    const inner = new Map(m.get(key) ?? []);
    for (const c of cursors) inner.set(c, byCursor.get(c) ?? new Map());
    return new Map(m).set(key, inner);
  });
  setCommentCounts((m) => {
    const out = new Map(m);
    for (const p of answer) {
      if (p.comments !== undefined) out.set(overlayKey(idHex, p.cursor), p.comments);
    }
    return out;
  });
  setVoteCounts((m) => {
    const out = new Map(m);
    for (const p of answer) {
      if (p.votes !== undefined) out.set(overlayKey(idHex, p.cursor), p.votes);
    }
    return out;
  });
  for (const c of cursors) {
    const k = overlayKey(idHex, c);
    const waits = settleAfter.get(k);
    if (waits === undefined) continue;
    for (const [emoji, after] of [...waits]) {
      if (issued > after) {
        waits.delete(emoji);
        clearOverlay(idHex, c, emoji);
      }
    }
    if (waits.size === 0) settleAfter.delete(k);
  }
}

/** Ask the counts of the posts in view; calls within a moment are joined
 *  into one request per 50 posts. */
export function requestReactionCounts(idHex: string, cursors: readonly number[]): void {
  const key = idHex.toLowerCase();
  const set = pending.get(key) ?? new Set<number>();
  for (const c of cursors) set.add(c);
  pending.set(key, set);
  if (flushTimer !== null) return;
  flushTimer = setTimeout(() => {
    flushTimer = null;
    const work = [...pending];
    pending.clear();
    for (const [channel, cs] of work) {
      for (const batch of countBatches([...cs])) void fetchBatch(channel, batch);
    }
  }, 50);
}

// ---- writing ------------------------------------------------------------

type WaitingReaction = WaitingReactionRecord;

const waiting: WaitingReaction[] = [];
let waitingTimer: ReturnType<typeof setTimeout> | null = null;
let identityForWaiting: (() => Identity | null) | null = null;

/** The account-kv row the waiting reactions are kept in, so a reaction drawn
 *  before a restart still leaves with the next token after it. */
export const WAITING_REACTIONS_ROW = "channel-reactions-waiting";

let waitingAccount: string | null = null;
let waitingGeneration = 0;

/** Keep the queue as it stands now (sealed, behind the store gate). */
function persistWaiting(): void {
  const account = waitingAccount;
  if (account === null) return;
  const value = waiting.map((r) => ({ idHex: r.idHex, cursor: r.cursor, epoch: r.epoch, emoji: r.emoji }));
  void saveAccountJson(account, WAITING_REACTIONS_ROW, value).catch((err: unknown) => {
    // eslint-disable-next-line no-console
    console.warn("[channels] keeping the waiting reactions failed", err instanceof Error ? err.name : "unknown");
  });
}

/**
 * The signed-in account (or `null`): drops what another account held and
 * reads back this one's waiting reactions, which then leave with the next
 * tokens. Idempotent.
 */
export function bindReactionAccount(accountIdB64: string | null): void {
  if (accountIdB64 === waitingAccount) return;
  waitingAccount = accountIdB64;
  waitingGeneration += 1;
  const generation = waitingGeneration;
  waiting.length = 0;
  setRawCounts(new Map());
  setCommentCounts(new Map());
  setOverlays(new Map());
  settleAfter.clear();
  if (waitingTimer !== null) clearTimeout(waitingTimer);
  waitingTimer = null;
  bindCommentReactionAccount(accountIdB64);
  if (accountIdB64 === null) return;
  void loadAccountJson(accountIdB64, WAITING_REACTIONS_ROW)
    .then((raw) => {
      if (generation !== waitingGeneration) return;
      for (const r of decodePersistedWaiting(raw)) {
        if (waiting.some((x) => x.idHex === r.idHex && x.cursor === r.cursor && x.emoji === r.emoji)) continue;
        waiting.push(r);
        bumpOverlay(r.idHex, r.cursor, r.emoji, 1);
      }
      void drainWaiting();
    })
    .catch(() => undefined);
}

/** A reaction cannot be sent yet for want of local state (the channel's
 *  seeds or the synced state are not loaded): it waits like a token. */
class NotReadyError extends Error {
  constructor() {
    super("channel reaction state not loaded yet");
    this.name = "NotReadyError";
  }
}

/** Whether an error means "no token to spend now": the reaction waits. */
export function isWaitForToken(err: unknown): boolean {
  if (err instanceof NotReadyError) return true;
  if (err instanceof FeedbackApiError) return err.status === 0 || err.status === 429 || err.status === 503;
  return anonTokenQuotaExhausted(err) !== null || isAnonTokenNetworkFailure(err) || err instanceof TypeError;
}

function labelAndKey(
  w: Wasm,
  identity: Identity,
  idHex: string,
  cursor: number,
  epoch: number,
  emoji: string,
): { label: Uint8Array; writeKey: InstanceType<Wasm["ChannelReactionWriteKey"]> } | null {
  const seed = channelSeedFor(idHex, epoch);
  if (seed === undefined) return null;
  const channelId = bytesOfHex(idHex);
  const rk = w.ChannelReactionKey.derive(seed, channelId, epoch);
  let label: Uint8Array;
  try {
    label = rk.label(BigInt(cursor), emoji);
  } finally {
    rk.free();
  }
  return { label, writeKey: new w.ChannelReactionWriteKey(identity, channelId, BigInt(cursor), label) };
}

async function sendAdd(identity: Identity, r: WaitingReaction): Promise<void> {
  const w = await loadWasm();
  const lk = labelAndKey(w, identity, r.idHex, r.cursor, r.epoch, r.emoji);
  if (lk === null) throw new NotReadyError();
  try {
    await addReactionRow(r.idHex, r.cursor, hexOf(lk.writeKey.publicKey), hexOf(lk.label), lk.writeKey.signAdd());
  } finally {
    lk.writeKey.free();
  }
}

async function sendDelete(identity: Identity, r: WaitingReaction): Promise<void> {
  const w = await loadWasm();
  const lk = labelAndKey(w, identity, r.idHex, r.cursor, r.epoch, r.emoji);
  if (lk === null) return;
  try {
    await deleteReactionRow(r.idHex, r.cursor, hexOf(lk.writeKey.publicKey), hexOf(lk.label), lk.writeKey.signDelete());
  } finally {
    lk.writeKey.free();
  }
}

function markSent(r: WaitingReaction): void {
  const k = overlayKey(r.idHex, r.cursor);
  const waits = settleAfter.get(k) ?? new Map<string, number>();
  waits.set(r.emoji, fetchIssue);
  settleAfter.set(k, waits);
  requestReactionCounts(r.idHex, [r.cursor]);
}

function scheduleWaiting(): void {
  if (waitingTimer !== null || waiting.length === 0) return;
  waitingTimer = setTimeout(() => {
    waitingTimer = null;
    void drainWaiting();
  }, WAITING_RETRY_MS);
}

async function drainWaiting(): Promise<void> {
  const identity = identityForWaiting?.() ?? null;
  // Before the synced state is loaded the own register reads empty, which
  // must not read as "taken off": wait for it.
  if (identity === null || ownFeedbackPort() === null) {
    scheduleWaiting();
    return;
  }
  const generation = waitingGeneration;
  while (waiting.length > 0 && generation === waitingGeneration) {
    const r = waiting[0];
    if (r === undefined) break;
    // Taken off meanwhile: nothing to send.
    if (!ownReactionsOn(bytesOfHex(r.idHex), r.cursor).includes(r.emoji)) {
      waiting.shift();
      persistWaiting();
      clearOverlay(r.idHex, r.cursor, r.emoji);
      continue;
    }
    try {
      await sendAdd(identity, r);
      if (generation !== waitingGeneration) return;
      waiting.shift();
      persistWaiting();
      markSent(r);
    } catch (err) {
      if (isWaitForToken(err)) break;
      if (generation !== waitingGeneration) return;
      // Refused for itself (the post is gone): dropped.
      waiting.shift();
      persistWaiting();
      clearOverlay(r.idHex, r.cursor, r.emoji);
    }
  }
  scheduleWaiting();
}

/** The identity the waiting queue sends under, read when it runs. */
export function bindReactionIdentity(read: () => Identity | null): void {
  identityForWaiting = read;
}

/** The identity reactions are written under (posts and comments alike), or
 *  `null` before one is bound or while signed out. */
export function reactionIdentity(): Identity | null {
  return identityForWaiting?.() ?? null;
}

/** Try the waiting reactions now (an identity or the synced state just
 *  became available). */
export function kickWaitingReactions(): void {
  kickWaitingCommentReactions();
  if (waiting.length === 0) return;
  if (waitingTimer !== null) clearTimeout(waitingTimer);
  waitingTimer = null;
  void drainWaiting();
}

/** Why a toggle did nothing, for the cap line. */
export class ChannelReactionCapError extends Error {
  constructor() {
    super("own reactions on this post are at the cap");
    this.name = "ChannelReactionCapError";
  }
}

/**
 * Add `emoji` on the post, or take it off when it is this person's. A fourth
 * reaction throws {@link ChannelReactionCapError} (the caller shows the cap
 * line). Adding with no token to spend keeps the reaction waiting; nothing is
 * thrown for that.
 */
export async function toggleChannelPostReaction(
  idHex: string,
  cursor: number,
  epoch: number,
  emoji: string,
  postAtMs: number,
): Promise<void> {
  const port = ownFeedbackPort();
  const identity = identityForWaiting?.() ?? null;
  if (port === null || identity === null) return;
  const channelId = bytesOfHex(idHex);
  const own = port.reactionsOn(channelId, cursor);
  const r: WaitingReaction = { idHex: idHex.toLowerCase(), cursor, epoch, emoji };
  if (own.includes(emoji)) {
    await port.setReaction(channelId, cursor, emoji, false, postAtMs);
    const queued = waiting.findIndex((x) => x.idHex === r.idHex && x.cursor === cursor && x.emoji === emoji);
    if (queued >= 0) {
      // Never sent: nothing to take off at the service.
      waiting.splice(queued, 1);
      persistWaiting();
      clearOverlay(idHex, cursor, emoji);
      return;
    }
    bumpOverlay(idHex, cursor, emoji, -1);
    try {
      await sendDelete(identity, r);
    } catch {
      // A failed take-off leaves the row; the next counts show it.
    }
    markSent(r);
    return;
  }
  let max = 3;
  try {
    max = wasmSync().channelReactionsPerPostMax();
  } catch {
    // The default.
  }
  if (own.length >= max) throw new ChannelReactionCapError();
  try {
    await port.setReaction(channelId, cursor, emoji, true, postAtMs);
  } catch {
    throw new ChannelReactionCapError();
  }
  bumpOverlay(idHex, cursor, emoji, 1);
  try {
    await sendAdd(identity, r);
    markSent(r);
  } catch (err) {
    if (isWaitForToken(err)) {
      waiting.push(r);
      persistWaiting();
      scheduleWaiting();
      return;
    }
    // Refused for itself: the register is put back, the chip goes.
    await port.setReaction(channelId, cursor, emoji, false, postAtMs).catch(() => undefined);
    clearOverlay(idHex, cursor, emoji);
  }
}

/** The person's recent reactions, for the candidates (local only). */
export function recentReactionsFor(accountIdB64: string | null): string[] {
  try {
    return loadRecentReactions(undefined, accountIdB64 ?? undefined);
  } catch {
    return [];
  }
}

/** Drop everything held (sign-out, tests). */
export function forgetPostFeedback(): void {
  waitingAccount = null;
  waitingGeneration += 1;
  setRawCounts(new Map());
  setCommentCounts(new Map());
  setOverlays(new Map());
  settleAfter.clear();
  waiting.length = 0;
  pending.clear();
  forgetCommentReactions();
  if (flushTimer !== null) clearTimeout(flushTimer);
  flushTimer = null;
  if (waitingTimer !== null) clearTimeout(waitingTimer);
  waitingTimer = null;
}
