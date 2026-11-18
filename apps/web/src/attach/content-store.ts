// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 — the polls and checklists of each conversation and everything
 * that changes them: votes, closes, ticks, added items, edits, anonymous
 * ballot rows, and an anonymous poll's electorate (the ring and its
 * versions). One sealed `account-kv` row per conversation
 * (`attach:<groupIdB64>`).
 *
 * The receive loop files here (`receive.ts`), never the view: the ops are not
 * timeline rows, they reach a chat that is not open as much as one that is,
 * and the journal will not hand them over twice (MLS forward secrecy). The
 * view folds on demand through the core (`foldPoll`, `foldChecklist`,
 * `foldPollBallots`), so the three clients cannot disagree on a tally.
 *
 * Pure state + a small persistence seam; the reducers are exported for tests.
 */
import { createSignal } from "solid-js";

import { registerAccountMemory } from "../auth/account-memory";
import { loadAccountJson, saveAccountJson } from "../storage/account-kv";
import type { PersistedAttachment } from "../chat/message-log-store";
import type {
  Article,
  ChecklistAddRecord,
  ChecklistCloseRecord,
  ChecklistEditRecord,
  ChecklistTickRecord,
  PollCloseRecord,
  PollVoteRecord,
  SignedChecklist,
  SignedPoll,
} from "../../wasm/lakko_ffi_wasm";

/** A verified anonymous ballot, cached (verifying costs a proof check). */
export interface VerifiedBallotRecord {
  readonly tag: string;
  readonly choices: readonly number[];
  readonly version: number;
}

/** One anonymous ballot row as the journal served it. */
export interface BallotRow {
  readonly messageId: number;
  readonly createdAtMs: number;
  /** Standard base64 of the sealed row (kept for the hand-over of old rows). */
  readonly rowB64: string;
  /** `undefined` = not yet verified (the ring version it names is not held
   *  yet); `null` = refused for good. */
  readonly verified?: VerifiedBallotRecord | null;
}

export interface PollEntry {
  readonly kind: "poll";
  readonly messageId: number;
  readonly createdAtMs: number;
  /** Standard base64 of the author's account key, resolved at receipt. */
  readonly authorKeyB64: string;
  readonly signed: SignedPoll;
  /** The author signature (and, for an anonymous poll, the ring) checked. */
  readonly verified: boolean;
  /** Anonymous polls: the ring with its versions, `PollRing.encode()` in
   *  standard base64 (the core owns the version bookkeeping). */
  readonly ringB64?: string;
  readonly votes: readonly PollVoteRecord[];
  readonly closes: readonly PollCloseRecord[];
  readonly ballots: readonly BallotRow[];
  /** A channel poll's frozen final tally, when closed by its publisher. */
  readonly frozenTally?: unknown;
  /** ADR-0090 section 3.11 — the state a history bundle carried (counts,
   *  voters, closed). Shown ALONE while the entry is `fromSnapshot`: the
   *  bundle's totals already include an unknown set of votes, so live votes
   *  are never added to them (that would count a vote twice). The hand-over
   *  brings the per-voter state and replaces the entry with the full fold. */
  readonly baseline?: { readonly counts: readonly number[]; readonly voters: number; readonly closed: boolean };
  /** Filed from a history bundle, before any hand-over. */
  readonly fromSnapshot?: true;
}

export interface ChecklistEntry {
  readonly kind: "checklist";
  readonly messageId: number;
  readonly createdAtMs: number;
  readonly authorKeyB64: string;
  readonly signed: SignedChecklist;
  readonly verified: boolean;
  readonly edits: readonly ChecklistEditRecord[];
  readonly adds: readonly ChecklistAddRecord[];
  readonly ticks: readonly ChecklistTickRecord[];
  readonly closes: readonly ChecklistCloseRecord[];
  /** The author's latest signed edit (what a hand-over carries). */
  readonly currentSigned?: SignedChecklist;
  /** Filed from a history bundle, before any hand-over. */
  readonly fromSnapshot?: true;
}

export type ContentEntry = PollEntry | ChecklistEntry;

/** An op whose target this device does not hold yet (it may arrive by a
 *  hand-over later), filed by target. */
export type HeldOp =
  | { readonly kind: "vote"; readonly target: number; readonly record: PollVoteRecord }
  | { readonly kind: "close"; readonly target: number; readonly record: PollCloseRecord }
  | { readonly kind: "tick"; readonly target: number; readonly record: ChecklistTickRecord }
  | { readonly kind: "add"; readonly target: number; readonly record: ChecklistAddRecord }
  | { readonly kind: "edit"; readonly target: number; readonly record: ChecklistEditRecord };

/** The latest edit of an article, by its target (ADR-0090 section 1: an
 *  edit replaces the whole article). Shown only when `editor` is the
 *  article's author. Attachments in the message-log persisted form. */
export interface ArticleEdit {
  readonly messageId: number;
  readonly editor: string;
  readonly article: Article;
  readonly attachments: readonly PersistedAttachment[];
}

export interface ConversationContent {
  readonly entries: Readonly<Record<string, ContentEntry>>;
  readonly held: readonly HeldOp[];
  readonly articleEdits?: Readonly<Record<string, ArticleEdit>>;
}

/** Keep the newest edit per article (by DS message id). */
export function withArticleEdit(state: ConversationContent, target: number, edit: ArticleEdit): ConversationContent {
  const key = String(target);
  const have = state.articleEdits?.[key];
  if (have !== undefined && have.messageId >= edit.messageId) return state;
  return { ...state, articleEdits: { ...(state.articleEdits ?? {}), [key]: edit } };
}

export const EMPTY_CONTENT: ConversationContent = { entries: {}, held: [] };

/** Bound on ops kept for targets this device does not hold. Chosen. */
export const MAX_HELD_OPS = 2_000;

// ----- reducers (pure) --------------------------------------------------

/** File a poll or checklist; ops already held for it are folded in. */
export function withEntry(state: ConversationContent, entry: ContentEntry): ConversationContent {
  const key = String(entry.messageId);
  if (state.entries[key] !== undefined) return state;
  let next: ContentEntry = entry;
  const rest: HeldOp[] = [];
  for (const op of state.held) {
    if (op.target !== entry.messageId) {
      rest.push(op);
      continue;
    }
    next = applyToEntry(next, op) ?? next;
  }
  return { entries: { ...state.entries, [key]: next }, held: rest };
}

/** File one op: on its target when held, else kept for later. */
export function withOp(state: ConversationContent, op: HeldOp): ConversationContent {
  const key = String(op.target);
  const entry = state.entries[key];
  if (entry === undefined) {
    const held = [...state.held, op];
    return { ...state, held: held.length > MAX_HELD_OPS ? held.slice(held.length - MAX_HELD_OPS) : held };
  }
  const next = applyToEntry(entry, op);
  if (next === null) return state;
  return { ...state, entries: { ...state.entries, [key]: next } };
}

/** Apply one op to its entry; `null` when it does not fit the entry's kind
 *  or is already there (re-delivery is idempotent by DS message id). */
export function applyToEntry(entry: ContentEntry, op: HeldOp): ContentEntry | null {
  const id = op.record.messageId;
  if (entry.kind === "poll") {
    if (op.kind === "vote") {
      if (entry.votes.some((v) => v.messageId === id)) return null;
      return { ...entry, votes: [...entry.votes, op.record] };
    }
    if (op.kind === "close") {
      if (entry.closes.some((c) => c.messageId === id)) return null;
      return { ...entry, closes: [...entry.closes, op.record] };
    }
    return null;
  }
  switch (op.kind) {
    case "tick":
      if (entry.ticks.some((x) => x.messageId === id)) return null;
      return { ...entry, ticks: [...entry.ticks, op.record] };
    case "add":
      if (entry.adds.some((x) => x.messageId === id)) return null;
      return { ...entry, adds: [...entry.adds, op.record] };
    case "edit":
      if (entry.edits.some((x) => x.messageId === id)) return null;
      return { ...entry, edits: [...entry.edits, op.record] };
    case "close":
      if (entry.closes.some((x) => x.messageId === id)) return null;
      return { ...entry, closes: [...entry.closes, { messageId: id, closer: op.record.closer }] };
    default:
      return null;
  }
}

/**
 * File one anonymous ballot row on its poll, idempotent by DS message id AND
 * by row bytes: the same row may arrive from the hand-over's old rows (under
 * a stand-in id below every real one) and again from the journal (under its
 * real id), and the real id wins.
 */
export function withBallotRow(entry: PollEntry, row: BallotRow): PollEntry {
  if (entry.ballots.some((b) => b.messageId === row.messageId)) return entry;
  const same = entry.ballots.find((b) => b.rowB64 === row.rowB64);
  if (same !== undefined) {
    if (same.messageId >= 0 || row.messageId < 0) return entry;
    const rest = entry.ballots.filter((b) => b !== same);
    const verified = row.verified !== undefined ? row.verified : same.verified;
    const merged: BallotRow = verified === undefined ? { ...row } : { ...row, verified };
    return { ...entry, ballots: [...rest, merged].sort((a, b) => a.messageId - b.messageId) };
  }
  return { ...entry, ballots: [...entry.ballots, row].sort((a, b) => a.messageId - b.messageId) };
}

/**
 * ADR-0090 sections 3.9 and 3.11 — the hand-over's per-voter state replaces a
 * poll or checklist filed from a history bundle: its records, plus every
 * live op this device received meanwhile (the fold orders them by DS id, so
 * a voter's latest vote and an item's latest tick win). The bundle's totals
 * are dropped, so nothing is counted twice.
 */
export function replacingSnapshot(held: ContentEntry, adopted: ContentEntry): ContentEntry {
  if (held.fromSnapshot !== true) return held;
  if (held.kind === "poll" && adopted.kind === "poll") {
    const votes = [...adopted.votes, ...held.votes.filter((v) => !adopted.votes.some((a) => a.messageId === v.messageId))];
    const closes = [...adopted.closes, ...held.closes.filter((c) => !adopted.closes.some((a) => a.messageId === c.messageId))];
    let next: PollEntry = { ...adopted, votes, closes, ballots: [] };
    for (const b of [...adopted.ballots, ...held.ballots]) next = withBallotRow(next, b);
    return next;
  }
  if (held.kind === "checklist" && adopted.kind === "checklist") {
    const live = <T extends { messageId: number; privileged?: boolean }>(xs: readonly T[], ys: readonly T[]): T[] =>
      [...ys, ...xs.filter((x) => x.privileged !== true && !ys.some((y) => y.messageId === x.messageId))];
    return {
      ...adopted,
      adds: live(held.adds, adopted.adds),
      ticks: live(held.ticks, adopted.ticks),
      edits: [...adopted.edits, ...held.edits],
      closes: [...adopted.closes, ...held.closes.filter((c) => !adopted.closes.some((a) => a.messageId === c.messageId))],
      ...(held.currentSigned === undefined ? {} : { currentSigned: held.currentSigned }),
    };
  }
  return held;
}

/** Accounts of a roster not yet in a ring, sorted bytewise: the one
 *  version a commit adds (ADR-0090 section 3.4; members who left stay). */
export function ringAdditions(ringHas: (keyB64: string) => boolean, rosterAccountKeys: readonly string[]): string[] {
  const fresh = [...new Set(rosterAccountKeys)].filter((k) => !ringHas(k));
  return fresh.sort(compareKeyB64);
}

/** Bytewise order of two base64 keys (the ring's order). */
export function compareKeyB64(a: string, b: string): number {
  const x = b64(a);
  const y = b64(b);
  const n = Math.min(x.length, y.length);
  for (let i = 0; i < n; i++) {
    const d = (x[i] ?? 0) - (y[i] ?? 0);
    if (d !== 0) return d;
  }
  return x.length - y.length;
}

function b64(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** Whether a poll or checklist is still open by its own records (a poll's
 *  time close is the fold's; this is the op close only). */
export function closedByOp(entry: ContentEntry): boolean {
  return entry.closes.length > 0;
}

// ----- the live store ---------------------------------------------------

const ROW = (groupIdB64: string): string => `attach:${groupIdB64}`;

const [version, setVersion] = createSignal(0);
let account: string | null = null;
const memory = new Map<string, ConversationContent>();
const loading = new Map<string, Promise<ConversationContent>>();
const writes = new Map<string, Promise<void>>();

/** The account whose rows are read; switching drops everything in memory. */
export function bindContentAccount(accountId: string | null): void {
  if (account === accountId) return;
  account = accountId;
  memory.clear();
  loading.clear();
  setVersion((v) => v + 1);
}

/** Read a conversation's content (loads once). */
export async function loadContent(groupIdB64: string): Promise<ConversationContent> {
  const have = memory.get(groupIdB64);
  if (have !== undefined) return have;
  const pending = loading.get(groupIdB64);
  if (pending !== undefined) return pending;
  const acct = account;
  const p = (async (): Promise<ConversationContent> => {
    if (acct === null) return EMPTY_CONTENT;
    try {
      const raw = (await loadAccountJson(acct, ROW(groupIdB64))) as ConversationContent | null;
      const value =
        raw !== null && typeof raw === "object" && typeof raw.entries === "object" && Array.isArray(raw.held)
          ? raw
          : EMPTY_CONTENT;
      if (account === acct && !memory.has(groupIdB64)) memory.set(groupIdB64, value);
      return memory.get(groupIdB64) ?? value;
    } catch {
      return EMPTY_CONTENT;
    } finally {
      loading.delete(groupIdB64);
    }
  })();
  loading.set(groupIdB64, p);
  return p;
}

/** The in-memory value, reactive (tracks the store version). `undefined`
 *  until {@link loadContent} ran for the conversation. */
export function contentOf(groupIdB64: string): ConversationContent | undefined {
  version();
  return memory.get(groupIdB64);
}

/** Apply `f` to a conversation's content and persist it; serial per
 *  conversation so two receives never write over each other. */
export async function updateContent(
  groupIdB64: string,
  f: (c: ConversationContent) => ConversationContent,
): Promise<ConversationContent> {
  const before = writes.get(groupIdB64) ?? Promise.resolve();
  let result: ConversationContent = EMPTY_CONTENT;
  const run = before.then(async () => {
    const current = await loadContent(groupIdB64);
    const next = f(current);
    result = next;
    if (next === current) return;
    memory.set(groupIdB64, next);
    setVersion((v) => v + 1);
    const acct = account;
    if (acct !== null) {
      try {
        await saveAccountJson(acct, ROW(groupIdB64), next);
      } catch (err) {
        console.warn("[attach] content row not saved", err);
      }
    }
  });
  writes.set(
    groupIdB64,
    run.catch(() => undefined),
  );
  await run;
  return result;
}

/** One entry, reactive. */
export function entryOf(groupIdB64: string, messageId: number): ContentEntry | undefined {
  return contentOf(groupIdB64)?.entries[String(messageId)];
}

registerAccountMemory("attach-content", (ids) => {
  if (account !== null && ids.includes(account)) bindContentAccount(null);
});
