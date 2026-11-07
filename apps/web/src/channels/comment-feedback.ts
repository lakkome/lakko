// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Reactions on channel comments (ADR-0087 section 13a, phase B; UX
 * `group-and-channel-privacy.md` 13.2): the same chips and the same control
 * as on a post, under a key of their own.
 *
 * - **Counts.** The comments in view under one post are asked in one call
 *   (at most 50 a call). A label is resolved by trying every emoji this app
 *   knows under the comment-reaction key of the post's epoch; an unknown label
 *   is dropped. A failed call leaves the last counts and says nothing.
 * - **Writing.** Each reaction has a key of its own
 *   (`ChannelCommentReactionWriteKey` over the account seed, channel, post,
 *   comment and label). Adding spends a `channel-react` token; with none to
 *   spend it waits and leaves later, the chip already drawn. Taking one off
 *   spends none.
 * - **Own reactions.** In the account's synced state, beside the post
 *   reactions (`own-feedback-register.ts`; the register may name a comment
 *   since persisted state version 12), so every device of the account
 *   highlights them. Never asked of the service.
 */
import { createSignal } from "solid-js";

import { loadAccountJson, saveAccountJson } from "../storage/account-kv";
import { ownCommentReactionsOn, ownFeedbackPort } from "./own-feedback-register";
import { loadWasm, wasmSync, type Wasm } from "../wasm";
import { channelSeedFor } from "./channel-access";
import {
  commentCountBatches,
  commentKey,
  decodeWaitingCommentReactions,
  resolveCommentLabelCounts,
  type CommentRef,
  type WaitingCommentReaction,
} from "./comment-feedback-rules";
import {
  addCommentReactionRow,
  bytesOfHex,
  deleteCommentReactionRow,
  fetchCommentReactionCounts,
  FeedbackApiError,
  hexOf,
} from "./feedback-api";
import { drawnCounts } from "./feedback-rules";
import { isWaitForToken, reactionIdentity } from "./post-feedback";

export { resolveCommentLabelCounts, commentKey, type CommentRef };

type Identity = InstanceType<Wasm["Identity"]>;

/** The account-kv row the waiting reactions are kept in (sealed, behind the
 *  store gate). */
export const WAITING_COMMENT_REACTIONS_ROW = "channel-comment-reactions-waiting";

/** Between tries to send a reaction that waits for a token (chosen, as for
 *  posts). */
const WAITING_RETRY_MS = 30_000;

const [rawCounts, setRawCounts] = createSignal<ReadonlyMap<string, ReadonlyMap<string, number>>>(new Map());
const [overlays, setOverlays] = createSignal<ReadonlyMap<string, ReadonlyMap<string, number>>>(new Map());
const settleAfter = new Map<string, Map<string, number>>();
let fetchIssue = 0;

/** `${channel}:${post}` -> comment cursors waiting to be asked. */
const pending = new Map<string, Set<number>>();
let flushTimer: ReturnType<typeof setTimeout> | null = null;

const waiting: WaitingCommentReaction[] = [];
let waitingTimer: ReturnType<typeof setTimeout> | null = null;
let account: string | null = null;
let generation = 0;

// ---- reads --------------------------------------------------------------

/** The raw label counts of one comment as last fetched. Reactive. */
export function commentLabelCounts(r: CommentRef): ReadonlyMap<string, number> | undefined {
  return rawCounts().get(commentKey(r));
}

/** This account's reactions on the comment (the synced register).
 *  Reactive. */
export function ownCommentReactions(r: CommentRef): readonly string[] {
  return ownCommentReactionsOn(bytesOfHex(r.idHex), r.post, r.comment);
}

/** The counts drawn for one comment: the service's resolved counts with this
 *  person's writes not yet seen added. Reactive through the caller's reads. */
export function drawnCommentCounts(r: CommentRef, resolved: ReadonlyMap<string, number>): Map<string, number> {
  return drawnCounts(resolved, ownCommentReactions(r), overlays().get(commentKey(r)) ?? new Map());
}

function bumpOverlay(k: string, emoji: string, delta: number): void {
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

function clearOverlay(k: string, emoji: string): void {
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

async function fetchBatch(idHex: string, post: number, comments: number[]): Promise<void> {
  fetchIssue += 1;
  const issued = fetchIssue;
  let answer;
  try {
    answer = await fetchCommentReactionCounts(idHex, post, comments);
  } catch {
    return;
  }
  const byCursor = new Map(answer.map((c) => [c.cursor, c.labels] as const));
  setRawCounts((m) => {
    const out = new Map(m);
    for (const c of comments) out.set(commentKey({ idHex, post, comment: c }), byCursor.get(c) ?? new Map());
    return out;
  });
  for (const c of comments) {
    const k = commentKey({ idHex, post, comment: c });
    const waits = settleAfter.get(k);
    if (waits === undefined) continue;
    for (const [emoji, after] of [...waits]) {
      if (issued > after) {
        waits.delete(emoji);
        clearOverlay(k, emoji);
      }
    }
    if (waits.size === 0) settleAfter.delete(k);
  }
}

/** Ask the counts of comments in view under one post; calls within a moment
 *  are joined into one request per 50 comments. */
export function requestCommentReactionCounts(idHex: string, post: number, comments: readonly number[]): void {
  const key = `${idHex.toLowerCase()}:${post}`;
  const set = pending.get(key) ?? new Set<number>();
  for (const c of comments) set.add(c);
  pending.set(key, set);
  if (flushTimer !== null) return;
  flushTimer = setTimeout(() => {
    flushTimer = null;
    const work = [...pending];
    pending.clear();
    for (const [k, cs] of work) {
      const sep = k.lastIndexOf(":");
      const id = k.slice(0, sep);
      const p = Number(k.slice(sep + 1));
      for (const batch of commentCountBatches([...cs])) void fetchBatch(id, p, batch);
    }
  }, 50);
}

// ---- keeping ------------------------------------------------------------

function persistWaiting(): void {
  const a = account;
  if (a === null) return;
  const value = waiting.map((r) => ({
    idHex: r.idHex,
    post: r.post,
    comment: r.comment,
    epoch: r.epoch,
    emoji: r.emoji,
    postAtMs: r.postAtMs,
  }));
  void saveAccountJson(a, WAITING_COMMENT_REACTIONS_ROW, value).catch((err: unknown) => {
    // eslint-disable-next-line no-console
    console.warn("[channels] keeping waiting comment reactions failed", err instanceof Error ? err.name : "unknown");
  });
}

/** Write the own register; `false` when the synced state is not loaded. */
async function setOwnReaction(r: WaitingCommentReaction, on: boolean): Promise<void> {
  const port = ownFeedbackPort();
  if (port === null) throw new NotReadyError();
  await port.setCommentReaction(bytesOfHex(r.idHex), r.post, r.comment, r.emoji, on, r.postAtMs);
}

/** Bind to the signed-in account (or `null`): drops what another account
 *  held and reads back this one's. Idempotent; called by `post-feedback`. */
export function bindCommentReactionAccount(accountIdB64: string | null): void {
  if (accountIdB64 === account) return;
  account = accountIdB64;
  generation += 1;
  const gen = generation;
  waiting.length = 0;
  setRawCounts(new Map());
  setOverlays(new Map());
  settleAfter.clear();
  if (waitingTimer !== null) clearTimeout(waitingTimer);
  waitingTimer = null;
  if (accountIdB64 === null) return;
  void loadAccountJson(accountIdB64, WAITING_COMMENT_REACTIONS_ROW)
    .then((rawWaiting) => {
      if (gen !== generation) return;
      for (const r of decodeWaitingCommentReactions(rawWaiting)) {
        if (waiting.some((x) => commentKey(x) === commentKey(r) && x.emoji === r.emoji)) continue;
        waiting.push(r);
        bumpOverlay(commentKey(r), r.emoji, 1);
      }
      void drainWaiting();
    })
    .catch(() => undefined);
}

// ---- writing ------------------------------------------------------------

class NotReadyError extends Error {
  constructor() {
    super("channel comment reaction state not loaded yet");
    this.name = "NotReadyError";
  }
}

function labelAndKey(
  w: Wasm,
  identity: Identity,
  r: WaitingCommentReaction,
): { label: Uint8Array; writeKey: InstanceType<Wasm["ChannelCommentReactionWriteKey"]> } | null {
  const seed = channelSeedFor(r.idHex, r.epoch);
  if (seed === undefined) return null;
  const channelId = bytesOfHex(r.idHex);
  const rk = w.ChannelCommentReactionKey.derive(seed, channelId, r.epoch);
  let label: Uint8Array;
  try {
    label = rk.label(BigInt(r.post), BigInt(r.comment), r.emoji);
  } finally {
    rk.free();
  }
  return {
    label,
    writeKey: new w.ChannelCommentReactionWriteKey(identity, channelId, BigInt(r.post), BigInt(r.comment), label),
  };
}

async function sendAdd(identity: Identity, r: WaitingCommentReaction): Promise<void> {
  const w = await loadWasm();
  const lk = labelAndKey(w, identity, r);
  if (lk === null) throw new NotReadyError();
  try {
    await addCommentReactionRow(
      r.idHex,
      r.post,
      r.comment,
      hexOf(lk.writeKey.publicKey),
      hexOf(lk.label),
      lk.writeKey.signAdd(),
    );
  } finally {
    lk.writeKey.free();
  }
}

async function sendDelete(identity: Identity, r: WaitingCommentReaction): Promise<void> {
  const w = await loadWasm();
  const lk = labelAndKey(w, identity, r);
  if (lk === null) return;
  try {
    await deleteCommentReactionRow(
      r.idHex,
      r.post,
      r.comment,
      hexOf(lk.writeKey.publicKey),
      hexOf(lk.label),
      lk.writeKey.signDelete(),
    );
  } finally {
    lk.writeKey.free();
  }
}

function isWait(err: unknown): boolean {
  return err instanceof NotReadyError || isWaitForToken(err);
}

function markSent(r: WaitingCommentReaction): void {
  const k = commentKey(r);
  const waits = settleAfter.get(k) ?? new Map<string, number>();
  waits.set(r.emoji, fetchIssue);
  settleAfter.set(k, waits);
  requestCommentReactionCounts(r.idHex, r.post, [r.comment]);
}

function scheduleWaiting(): void {
  if (waitingTimer !== null || waiting.length === 0) return;
  waitingTimer = setTimeout(() => {
    waitingTimer = null;
    void drainWaiting();
  }, WAITING_RETRY_MS);
}

async function drainWaiting(): Promise<void> {
  const identity = reactionIdentity();
  // Before the synced state is loaded the own register reads empty, which
  // must not read as "taken off": wait for it.
  if (identity === null || ownFeedbackPort() === null) {
    scheduleWaiting();
    return;
  }
  const gen = generation;
  while (waiting.length > 0 && gen === generation) {
    const r = waiting[0];
    if (r === undefined) break;
    if (!ownCommentReactions(r).includes(r.emoji)) {
      waiting.shift();
      persistWaiting();
      clearOverlay(commentKey(r), r.emoji);
      continue;
    }
    try {
      await sendAdd(identity, r);
      if (gen !== generation) return;
      waiting.shift();
      persistWaiting();
      markSent(r);
    } catch (err) {
      if (isWait(err)) break;
      if (gen !== generation) return;
      // Refused for itself (the comment is gone): dropped.
      waiting.shift();
      persistWaiting();
      clearOverlay(commentKey(r), r.emoji);
      await setOwnReaction(r, false).catch(() => undefined);
    }
  }
  scheduleWaiting();
}

/** Try the waiting comment reactions now. */
export function kickWaitingCommentReactions(): void {
  if (waiting.length === 0) return;
  if (waitingTimer !== null) clearTimeout(waitingTimer);
  waitingTimer = null;
  void drainWaiting();
}

/** A fourth own reaction on one comment. */
export class CommentReactionCapError extends Error {
  constructor() {
    super("own reactions on this comment are at the cap");
    this.name = "CommentReactionCapError";
  }
}

/** The cap per comment: the per-post cap of the core (3). */
export function commentReactionCap(): number {
  try {
    return wasmSync().channelReactionsPerPostMax();
  } catch {
    return 3;
  }
}

/**
 * Add `emoji` on the comment, or take it off when it is this person's. A
 * fourth throws {@link CommentReactionCapError}. With no token to spend the
 * reaction waits; nothing is thrown for that.
 */
export async function toggleCommentReaction(
  r: CommentRef & { readonly epoch: number; readonly postAtMs: number },
  emoji: string,
): Promise<void> {
  // Sends need the identity and the synced register only, as for posts; the
  // account binding keeps only the waiting queue's row (a reaction made before
  // it is bound waits in memory).
  const identity = reactionIdentity();
  if (identity === null || ownFeedbackPort() === null) {
    console.warn("[channels] comment reaction: identity or synced state not ready");
    return;
  }
  const w: WaitingCommentReaction = {
    idHex: r.idHex.toLowerCase(),
    post: r.post,
    comment: r.comment,
    epoch: r.epoch,
    emoji,
    postAtMs: r.postAtMs,
  };
  const k = commentKey(w);
  const mine = ownCommentReactions(w);
  if (mine.includes(emoji)) {
    await setOwnReaction(w, false);
    const queued = waiting.findIndex((x) => commentKey(x) === k && x.emoji === emoji);
    if (queued >= 0) {
      waiting.splice(queued, 1);
      persistWaiting();
      clearOverlay(k, emoji);
      return;
    }
    bumpOverlay(k, emoji, -1);
    try {
      await sendDelete(identity, w);
    } catch {
      // A failed take-off leaves the row; the next counts show it.
    }
    markSent(w);
    return;
  }
  if (mine.length >= commentReactionCap()) throw new CommentReactionCapError();
  try {
    await setOwnReaction(w, true);
  } catch (err) {
    // Only the register's `bounds:` refusal (a fourth, another device's
    // write) is the cap; anything else is a failure, never shown as the cap.
    if (err instanceof Error && err.message.startsWith("bounds:")) throw new CommentReactionCapError();
    throw err;
  }
  bumpOverlay(k, emoji, 1);
  try {
    await sendAdd(identity, w);
    markSent(w);
  } catch (err) {
    if (isWait(err)) {
      waiting.push(w);
      persistWaiting();
      scheduleWaiting();
      return;
    }
    // Refused for itself (the comment is gone, 404 unknown-comment): the
    // register is put back.
    await setOwnReaction(w, false).catch(() => undefined);
    clearOverlay(k, emoji);
    if (err instanceof FeedbackApiError && err.status === 404) return;
  }
}

/** Drop everything held (sign-out, tests). */
export function forgetCommentReactions(): void {
  account = null;
  generation += 1;
  setRawCounts(new Map());
  setOverlays(new Map());
  settleAfter.clear();
  waiting.length = 0;
  pending.clear();
  if (flushTimer !== null) clearTimeout(flushTimer);
  flushTimer = null;
  if (waitingTimer !== null) clearTimeout(waitingTimer);
  waitingTimer = null;
}
