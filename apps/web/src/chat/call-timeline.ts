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
 * The timeline's own row objects are stable across a `setRows`, and the point
 * of this module is to preserve that, so each row and each call keeps ONE
 * wrapper for as long as it exists (a `WeakMap`, so a row that leaves the
 * conversation takes its wrapper with it).
 *
 * This is memoization, not state: the same row object always produces the same
 * wrapper, and the wrapper's content depends on nothing but that row.
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

/** One entry of the merged conversation. */
export type ConversationEntry<Row> =
  | { readonly kind: "message"; readonly row: Row }
  | { readonly kind: "call"; readonly call: CallLogEntry };

/** One wrapper per row / call object, for as long as it exists. */
const messageWrappers = new WeakMap<object, unknown>();
const callWrappers = new WeakMap<CallLogEntry, unknown>();

function messageEntry<Row extends object>(row: Row): ConversationEntry<Row> {
  const cached = messageWrappers.get(row);
  if (cached !== undefined) return cached as ConversationEntry<Row>;
  const made: ConversationEntry<Row> = { kind: "message", row };
  messageWrappers.set(row, made);
  return made;
}

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
 * `rows` must already be oldest-first (the timeline's own order); `calls` may
 * be in any order. The message rows come out in exactly their input order.
 */
export function mergeConversationEntries<Row extends object>(
  rows: readonly Row[],
  calls: readonly CallLogEntry[],
  timeOf: RowTimeOf<Row>,
): ConversationEntry<Row>[] {
  if (calls.length === 0) return rows.map((row) => messageEntry(row));
  const times = effectiveTimes(rows, timeOf);
  const pending = [...calls].sort((a, b) => a.startedAtMs - b.startedAtMs);
  const out: ConversationEntry<Row>[] = [];
  let next = 0;
  for (let i = 0; i < rows.length; i += 1) {
    const at = times[i] ?? Number.NEGATIVE_INFINITY;
    // Strictly-less, so a call that ties with a message is drawn after it.
    while (next < pending.length && (pending[next] as CallLogEntry).startedAtMs < at) {
      out.push(callEntry(pending[next] as CallLogEntry));
      next += 1;
    }
    out.push(messageEntry(rows[i] as Row));
  }
  for (; next < pending.length; next += 1) {
    out.push(callEntry(pending[next] as CallLogEntry));
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
