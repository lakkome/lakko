// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 9 (live walk 2026-11-26, D4) - the founder of an owned
 * group's successor serves the reset person's history before it hands them
 * the ownership back (`continuity-runtime.ts`), when it was no admin of the
 * old group: a history page, and the carried name, are taken only from an
 * owner or an admin. Per account: successor group id (base64) -> when the
 * founder first held the hand-over back, and whether the history was served
 * (an empty page, the requester's last, was posted). The same rule as iOS
 * (`HistoryServedStore`).
 */

/** How long the hand-over waits for the history at most (chosen): the reset
 *  person may never ask (an older client, or not online). */
export const HISTORY_SERVED_WAIT_MS = 3 * 24 * 60 * 60 * 1000;

export interface HistoryServedEntry {
  readonly firstHeldMs: number;
  readonly served: boolean;
}

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
  return `lakko:history-served:${accountIdB64}`;
}

/**
 * | served | held back since       | due |
 * |--------|-----------------------|-----|
 * | yes    | -                     | yes |
 * | no     | never (now recorded)  | no  |
 * | no     | less than the wait    | no  |
 * | no     | the wait or more      | yes |
 *
 * A clock that went back reads as just held back. Pure.
 */
export function historyTransferDue(entry: HistoryServedEntry | undefined, nowMs: number): boolean {
  if (entry === undefined) return false;
  if (entry.served) return true;
  return nowMs >= entry.firstHeldMs && nowMs - entry.firstHeldMs >= HISTORY_SERVED_WAIT_MS;
}

/** Every entry of one account. */
export function readHistoryServed(
  accountIdB64: string,
  s: KeyValueStorage | undefined = storage(),
): Record<string, HistoryServedEntry> {
  try {
    const raw = s?.getItem(key(accountIdB64));
    if (raw === null || raw === undefined) return {};
    const parsed = JSON.parse(raw) as Record<string, Partial<HistoryServedEntry>>;
    const out: Record<string, HistoryServedEntry> = {};
    for (const [g, v] of Object.entries(parsed)) {
      if (typeof v.firstHeldMs !== "number" || !Number.isFinite(v.firstHeldMs)) continue;
      out[g] = { firstHeldMs: v.firstHeldMs, served: v.served === true };
    }
    return out;
  } catch {
    return {};
  }
}

function write(accountIdB64: string, all: Record<string, HistoryServedEntry>, s: KeyValueStorage | undefined): void {
  try {
    s?.setItem(key(accountIdB64), JSON.stringify(all));
  } catch {
    /* the wait starts again at the next pass; it only delays the hand-over */
  }
}

/** Whether the hand-over of `successorGroupIdB64` is due; the first ask
 *  starts the wait. */
export function historyTransferDueFor(
  accountIdB64: string,
  successorGroupIdB64: string,
  nowMs: number,
  s: KeyValueStorage | undefined = storage(),
): boolean {
  const all = readHistoryServed(accountIdB64, s);
  const entry = all[successorGroupIdB64];
  if (entry === undefined) {
    write(accountIdB64, { ...all, [successorGroupIdB64]: { firstHeldMs: nowMs, served: false } }, s);
    return false;
  }
  return historyTransferDue(entry, nowMs);
}

/** The history of `successorGroupIdB64` was served. */
export function markHistoryServed(
  accountIdB64: string,
  successorGroupIdB64: string,
  nowMs: number,
  s: KeyValueStorage | undefined = storage(),
): void {
  const all = readHistoryServed(accountIdB64, s);
  const entry = all[successorGroupIdB64];
  if (entry?.served === true) return;
  write(accountIdB64, { ...all, [successorGroupIdB64]: { firstHeldMs: entry?.firstHeldMs ?? nowMs, served: true } }, s);
}

/** Wipe. */
export function clearHistoryServed(accountIdB64: string, s: KeyValueStorage | undefined = storage()): void {
  try {
    s?.removeItem(key(accountIdB64));
  } catch {
    /* nothing recorded either */
  }
}
