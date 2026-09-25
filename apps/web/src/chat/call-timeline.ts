// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Where a call row sits in the conversation (`docs/ux/calls.md` §6).
 *
 * The timeline's rows come off the DS journal in the order the server assigned
 * them; call rows come off this device's own local log (`calls/call-log.ts`)
 * and were never on the wire at all. They still have to appear in one list, in
 * time order, because a conversation is one thing to the person reading it.
 *
 * This module is that merge, and nothing else — no Solid, no IndexedDB, no
 * wasm, so vitest can pin it. The renderer in `routes/Chat.tsx` walks the
 * result; `chat/timeline-layout.ts` computes day separators and message runs
 * over the same list, which is why the merge happens **before** the layout
 * rather than after: a call row on a new day must be able to carry that day's
 * separator, and a call between two of someone's messages must break the run
 * (`layoutTimeline` already does that for a `system` row).
 *
 * ## Ordering, and the two cases that are not obvious
 *
 * Message rows arrive oldest-first and carry `createdAt`, the DS row's
 * server-assigned stamp. Call rows carry `startedAtMs`, a local clock reading.
 * The two clocks are not the same clock, and neither is authoritative over the
 * other — so this is a best-effort interleave, not a total order anyone should
 * reason about for correctness. Nothing depends on it beyond where a row is
 * drawn.
 *
 * 1. **A message with no stamp inherits the previous row's.** An outgoing
 *    bubble the DS has not acknowledged yet has no `createdAt`; it is the
 *    newest thing in the conversation and must stay pinned after the row it
 *    followed, not float to the top because `undefined` sorted low.
 * 2. **A tie goes to the message.** Arbitrary, but fixed: an unstable answer
 *    would let a row jump on re-render.
 *
 * ## The wrappers are stable, and that is load-bearing
 *
 * Solid's `<For>` reconciles by reference identity: a list of freshly built
 * wrapper objects looks like an entirely new list on every recompute, so every
 * bubble in the conversation would be torn down and rebuilt each time a
 * message arrived — losing playing video, open previews and scroll position.
 * So each row and each call keeps ONE wrapper for as long as it exists.
 *
 * A wrapper is keyed on the row's IDENTITY (`rowIdentity` — a `tempId` or a DS
 * `messageId`), not on the row object, and THAT is the correction this module
 * carried a defect for. A row object is immutable and replaced on every state
 * change: `{...r, ack}` on the ack flip, and a whole new row when an upload
 * lands. Keyed on the object, every one of those replacements produced a new
 * wrapper and rebuilt the bubble — measured: a person playing back their own
 * voice message lost the sound the moment the send landed, because the element
 * making it was inside a bubble that had just been discarded. (The old text here
 * said "the timeline's own row objects are stable across a `setRows`". That
 * holds for every row the update did not touch, and is false for exactly the one
 * it did — which is the row the bubble is about.)
 *
 * The other half of the same correction: `entry.row` is a LIVE read, which is
 * why the rows arrive as a FUNCTION rather than as an array. A wrapper that
 * outlives a replacement has to hand out the CURRENT row, or a bubble that is
 * no longer rebuilt would go on showing the row it was built with — reading
 * `Not sent` under a message that has been sent. Where the function is a
 * reactive read (Solid's signal accessor, in `routes/Chat.tsx`), reading
 * `entry.row` inside a computation tracks it, so the renderer updates the
 * bubble instead of replacing it.
 *
 * This is memoization, not state: what a wrapper hands out depends on nothing
 * but the rows it is asked about.
 */

import type { CallLogEntry } from "../calls/call-log";

/**
 * When a message row happened, as an ISO stamp — or `undefined` for a row the
 * DS has not acknowledged yet.
 *
 * A function rather than a `createdAt` field on the row, because the timeline's
 * own row type keeps the stamp in two different places depending on the
 * variant (`chat/timeline-row.ts` `rowCreatedAt` is the reader). Asking the
 * caller for the reader keeps this module from having to know the row shape at
 * all, which is also what makes it testable against a row type of the test's
 * own choosing.
 */
export type RowTimeOf<Row> = (row: Row) => string | undefined;

/** Which row a row IS, as something that outlives the row object
 *  (`timeline-row.ts` `rowIdentity` is the timeline's own). */
export type RowIdentityOf<Row> = (row: Row) => string;

/** One entry of the merged conversation. */
export type ConversationEntry<Row> =
  | { readonly kind: "message"; readonly row: Row }
  | { readonly kind: "call"; readonly call: CallLogEntry };

/**
 * One conversation's bookkeeping, keyed on the rows accessor — which is one
 * object per conversation for its lifetime, and is also what makes two
 * conversations' identities (`out:1` in both) impossible to confuse.
 */
interface Bookkeeping<Row> {
  /** One wrapper per row identity, for as long as that row is in the list. */
  readonly wrappers: Map<string, ConversationEntry<Row>>;
  /** The rows array `byId` was built from, so it is rebuilt exactly when the
   *  list changes rather than on every read. */
  indexed: readonly Row[] | null;
  byId: Map<string, Row>;
}

const books = new WeakMap<object, Bookkeeping<never>>();

function bookkeeping<Row extends object>(rows: () => readonly Row[]): Bookkeeping<Row> {
  const existing = books.get(rows);
  if (existing !== undefined) return existing as unknown as Bookkeeping<Row>;
  const made: Bookkeeping<Row> = { wrappers: new Map(), indexed: null, byId: new Map() };
  books.set(rows, made as unknown as Bookkeeping<never>);
  return made;
}

/** The row `id` names, as it is NOW. Reading `rows()` here is what makes
 *  `entry.row` reactive for a caller whose accessor is a signal. */
function currentRow<Row extends object>(
  book: Bookkeeping<Row>,
  rows: () => readonly Row[],
  identityOf: RowIdentityOf<Row>,
  id: string,
): Row | undefined {
  const current = rows();
  if (book.indexed !== current) {
    const byId = new Map<string, Row>();
    for (const row of current) byId.set(identityOf(row), row);
    book.indexed = current;
    book.byId = byId;
  }
  return book.byId.get(id);
}

function messageEntry<Row extends object>(
  book: Bookkeeping<Row>,
  rows: () => readonly Row[],
  identityOf: RowIdentityOf<Row>,
  row: Row,
): ConversationEntry<Row> {
  const id = identityOf(row);
  const cached = book.wrappers.get(id);
  if (cached !== undefined) return cached;
  // The last row seen under this identity, so a read taken after the row has
  // left the conversation answers with what it last was rather than throwing.
  // Nothing renders such an entry — it is no longer in the list.
  let last = row;
  const made: ConversationEntry<Row> = {
    kind: "message",
    get row(): Row {
      const now = currentRow(book, rows, identityOf, id);
      if (now !== undefined) last = now;
      return last;
    },
  };
  book.wrappers.set(id, made);
  return made;
}

/** One wrapper per call row object, for as long as it exists. */
const callWrappers = new WeakMap<CallLogEntry, unknown>();

function callEntry<Row>(call: CallLogEntry): ConversationEntry<Row> {
  const cached = callWrappers.get(call);
  if (cached !== undefined) return cached as ConversationEntry<Row>;
  const made: ConversationEntry<Row> = { kind: "call", call };
  callWrappers.set(call, made);
  return made;
}

function millis(iso: string | undefined): number | null {
  if (iso === undefined) return null;
  const t = new Date(iso).getTime();
  return Number.isNaN(t) ? null : t;
}

/**
 * The effective time of every message row, carrying the last known stamp
 * forward over rows that have none.
 *
 * A leading run of unstamped rows has nothing to carry forward and gets
 * `-Infinity`, which keeps them at the top where they already were rather than
 * pushing them past every call row.
 */
function effectiveTimes<Row>(rows: readonly Row[], timeOf: RowTimeOf<Row>): number[] {
  const out: number[] = [];
  let last = Number.NEGATIVE_INFINITY;
  for (const row of rows) {
    const t = millis(timeOf(row));
    if (t !== null) last = t;
    out.push(last);
  }
  return out;
}

/**
 * Interleave a conversation's message rows with its call rows, oldest first.
 *
 * `rows` is a FUNCTION returning the conversation's rows as they are now — see
 * the module doc; it must return them oldest-first (the timeline's own order),
 * and `calls` may be in any order. The message rows come out in exactly their
 * input order.
 */
export function mergeConversationEntries<Row extends object>(
  rows: () => readonly Row[],
  calls: readonly CallLogEntry[],
  timeOf: RowTimeOf<Row>,
  identityOf: RowIdentityOf<Row>,
): ConversationEntry<Row>[] {
  const book = bookkeeping(rows);
  const current = rows();
  const out: ConversationEntry<Row>[] = [];
  const entry = (row: Row): ConversationEntry<Row> => messageEntry(book, rows, identityOf, row);
  if (calls.length === 0) {
    for (const row of current) out.push(entry(row));
  } else {
    const times = effectiveTimes(current, timeOf);
    const pending = [...calls].sort((a, b) => a.startedAtMs - b.startedAtMs);
    let next = 0;
    for (let i = 0; i < current.length; i += 1) {
      const at = times[i] ?? Number.NEGATIVE_INFINITY;
      // Strictly-less, so a call that ties with a message is drawn after it.
      while (next < pending.length && (pending[next] as CallLogEntry).startedAtMs < at) {
        out.push(callEntry(pending[next] as CallLogEntry));
        next += 1;
      }
      out.push(entry(current[i] as Row));
    }
    for (; next < pending.length; next += 1) {
      out.push(callEntry(pending[next] as CallLogEntry));
    }
  }
  // A row that has left the conversation takes its wrapper with it, which is
  // what the `WeakMap` on the row object used to do for free. Without this the
  // map would hold every row the conversation has ever shown.
  if (book.wrappers.size > current.length) {
    const live = new Set(current.map((row) => identityOf(row)));
    for (const id of [...book.wrappers.keys()]) {
      if (!live.has(id)) book.wrappers.delete(id);
    }
  }
  return out;
}

/** The ISO stamp a merged entry is shown under, for the day separator. */
export function entryCreatedAt<Row extends object>(
  entry: ConversationEntry<Row>,
  timeOf: RowTimeOf<Row>,
): string | undefined {
  if (entry.kind === "call") return new Date(entry.call.startedAtMs).toISOString();
  return timeOf(entry.row);
}
