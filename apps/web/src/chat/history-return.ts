// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 9 - the history comes back after a key reset, newest
 * first.
 *
 * ## The requester (the reset device)
 *
 * Once it joined a conversation inside its own continuity window (every
 * conversation it joins then is a returned one: the reset identity knows
 * nobody else), it asks there for history: a field-less request
 * (`composeHistoryRequestEnvelope`) carrying a fresh ADR-0091 gap key and
 * `before` (0: the newest). The answer is read by the ADR-0091 gap look
 * (`reentry-history.ts`, one look per group, the gap secret sealed under the
 * device key), whose signer rule is the core's `gapHistorySignerAccepted`.
 * Each page filed moves `before` to the oldest DS id it carried and asks
 * again; an empty page ends it ({@link nextHistoryStep}).
 *
 * ## The answerer
 *
 * {@link historyAnswerAllowed}: only for a request from the CURRENT key of
 * an account whose reset this device accepted, whose retired key was in the
 * conversation's earlier group this device holds; in a one-to-one the other
 * person answers, in a group an owner or admin. The page is the earlier
 * group's rows below `before`, {@link HISTORY_PAGE_SIZE} at most, sealed to
 * the request's gap key for the successor (`devices/history-bundle.ts`
 * `buildHistoryPage`), posted to `gapHistoryRendezvous(gap key)`. Only what
 * the sender still holds comes back. A member who joined later cannot ask
 * for history from before they were in: they were never in the earlier
 * group.
 */
import type { ConversationKind } from "./conversation-kind";

/** Rows per page (chosen, not measured). */
export const HISTORY_PAGE_SIZE = 200;

/** Pages asked for at most per conversation (200 x 50 = 10 000 rows;
 *  chosen, a stop for a peer that never sends an empty page). */
export const HISTORY_MAX_PAGES = 50;

/**
 * Whether this device answers one history request.
 *
 * | from self | requester's reset accepted | was in the earlier group | earlier group held | kind  | own role       | answers |
 * |-----------|----------------------------|--------------------------|--------------------|-------|----------------|---------|
 * | yes       | -                          | -                        | -                  | -     | -              | no      |
 * | no        | no                         | -                        | -                  | -     | -              | no      |
 * | no        | yes                        | no                       | -                  | -     | -              | no      |
 * | no        | yes                        | yes                      | no                 | -     | -              | no      |
 * | no        | yes                        | yes                      | yes                | chat  | -              | yes     |
 * | no        | yes                        | yes                      | yes                | group | owner or admin | yes     |
 * | no        | yes                        | yes                      | yes                | group | member         | no      |
 */
export function historyAnswerAllowed(facts: {
  readonly fromSelf: boolean;
  readonly requesterResetAccepted: boolean;
  readonly requesterWasInEarlierGroup: boolean;
  readonly earlierGroupHeld: boolean;
  readonly kind: ConversationKind;
  readonly ownIsOwnerOrAdmin: boolean;
}): boolean {
  if (facts.fromSelf || !facts.requesterResetAccepted) return false;
  if (!facts.requesterWasInEarlierGroup || !facts.earlierGroupHeld) return false;
  return facts.kind === "chat" || facts.ownIsOwnerOrAdmin;
}

/** The requester's state for one conversation. */
export interface HistoryRequestState {
  /** The next request's `before` (decimal string; "0" = the newest). */
  readonly before: string;
  /** Pages asked for so far. */
  readonly pages: number;
  /** When the current request was posted (unix ms); 0 = not yet. */
  readonly sentAtMs: number;
  /** How many times the current page was asked for with no answer. Absent
   *  in rows written before the field: 0. */
  readonly asks?: number;
  /** No more pages (an empty page came, or the page cap). */
  readonly done: boolean;
}

export const FIRST_HISTORY_REQUEST: HistoryRequestState = { before: "0", pages: 0, sentAtMs: 0, done: false };

/** A request with no page after this long is posted again (chosen). */
export const HISTORY_RESEND_MS = 10 * 60 * 1000;

/** A page asked for this many times with no answer is not asked for again:
 *  nobody holds an earlier group for it (a person met only after the reset),
 *  or nobody who may answer is online. Chosen. */
export const HISTORY_MAX_ASKS = 6;

/** The state after one more post of the current request. Pure. */
export function historyAsked(state: HistoryRequestState, nowMs: number): HistoryRequestState {
  return { ...state, sentAtMs: nowMs, asks: (state.asks ?? 0) + 1 };
}

/** Whether the current request is to be posted now. Pure. */
export function historyRequestDue(state: HistoryRequestState, nowMs: number): boolean {
  if (state.done || (state.asks ?? 0) >= HISTORY_MAX_ASKS) return false;
  return state.sentAtMs === 0 || nowMs < state.sentAtMs || nowMs - state.sentAtMs >= HISTORY_RESEND_MS;
}

/**
 * What a filed page does to the state: the oldest DS id among its rows is
 * the next `before`; an empty page, or one whose oldest id is not below the
 * current `before`, or the page cap, ends it. Pure.
 */
export function nextHistoryStep(state: HistoryRequestState, filedMessageIds: readonly string[]): HistoryRequestState {
  if (state.done) return state;
  let oldest: bigint | null = null;
  for (const id of filedMessageIds) {
    if (!/^[0-9]+$/.test(id)) continue;
    const v = BigInt(id);
    if (v > 0n && (oldest === null || v < oldest)) oldest = v;
  }
  const before = BigInt(state.before);
  const pages = state.pages + 1;
  if (oldest === null || (before > 0n && oldest >= before) || pages >= HISTORY_MAX_PAGES) {
    return { ...state, pages, done: true };
  }
  return { before: oldest.toString(), pages, sentAtMs: 0, asks: 0, done: false };
}

// ----------------------------------------------------------------------------
// The requester's states (localStorage, per account)
// ----------------------------------------------------------------------------

interface KeyValueStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function storage(): KeyValueStorage | undefined {
  try {
    return globalThis.localStorage ?? undefined;
  } catch {
    return undefined;
  }
}

function key(accountIdB64: string): string {
  return `lakko:history-return:${accountIdB64}`;
}

/** Every conversation's state. */
export function readHistoryStates(
  accountIdB64: string,
  s: KeyValueStorage | undefined = storage(),
): Record<string, HistoryRequestState> {
  try {
    const raw = s?.getItem(key(accountIdB64));
    if (raw === null || raw === undefined) return {};
    const parsed = JSON.parse(raw) as Record<string, Partial<HistoryRequestState>>;
    const out: Record<string, HistoryRequestState> = {};
    for (const [g, v] of Object.entries(parsed)) {
      if (typeof v.before !== "string" || !/^[0-9]+$/.test(v.before) || typeof v.pages !== "number") continue;
      const sentAtMs = typeof v.sentAtMs === "number" && Number.isFinite(v.sentAtMs) ? v.sentAtMs : 0;
      const asks = typeof v.asks === "number" && Number.isFinite(v.asks) ? v.asks : 0;
      out[g] = { before: v.before, pages: v.pages, sentAtMs, asks, done: v.done === true };
    }
    return out;
  } catch {
    return {};
  }
}

/** Write one conversation's state. */
export function writeHistoryState(
  accountIdB64: string,
  groupIdB64: string,
  state: HistoryRequestState,
  s: KeyValueStorage | undefined = storage(),
): void {
  const all = readHistoryStates(accountIdB64, s);
  all[groupIdB64] = state;
  try {
    s?.setItem(key(accountIdB64), JSON.stringify(all));
  } catch {
    /* asked again next poll; a duplicate page files once */
  }
}

/** Start asking for a returned conversation's history (idempotent). */
export function noteReturnedConversation(
  accountIdB64: string,
  groupIdB64: string,
  s: KeyValueStorage | undefined = storage(),
): void {
  if (readHistoryStates(accountIdB64, s)[groupIdB64] !== undefined) return;
  writeHistoryState(accountIdB64, groupIdB64, FIRST_HISTORY_REQUEST, s);
}

/** Wipe. */
export function clearHistoryStates(accountIdB64: string, s: KeyValueStorage | undefined = storage()): void {
  try {
    s?.removeItem(key(accountIdB64));
  } catch {
    /* nothing recorded either */
  }
}

// ----------------------------------------------------------------------------
// Receipt: the chat client hands a request on
// ----------------------------------------------------------------------------

/** One history request, at receipt. */
export interface HistoryRequestArrival {
  readonly groupIdB64: string;
  /** The sender's ACCOUNT key resolved at receipt (C-68). */
  readonly senderKeyB64: string;
  readonly gapKeyPub: Uint8Array;
  readonly beforeMessageId: bigint;
}

let handler: ((a: HistoryRequestArrival) => void) | null = null;

/** Install (or remove) what answers a request. */
export function installHistoryRequestHandler(fn: ((a: HistoryRequestArrival) => void) | null): void {
  handler = fn;
}

/** The chat client: one request arrived. A request that arrives with no
 *  handler installed is lost (the requester asks again only for a new page;
 *  its first ask is re-sent while unanswered, at its next poll). */
export function deliverHistoryRequest(a: HistoryRequestArrival): void {
  try {
    handler?.(a);
  } catch {
    /* an answer never fails the receive loop */
  }
}
