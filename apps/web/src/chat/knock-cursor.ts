// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0053 contract point 7 — the position an owner / admin device has read a
 * group's KNOCK queue up to.
 *
 * Its own cursor, separate from the journal's: the knock queue is a different
 * DS table with its own id sequence, so a journal id means nothing in it.
 *
 * Persisted per group in `localStorage` rather than kept in memory, because
 * re-feeding every knock after a reload would resurface a request the admin
 * already REJECTED (a reject is a local discard of the retained proposal; the
 * knock row stays on the DS for its seven days, and at an unchanged epoch the
 * proposal would apply again). Lagging is harmless — a re-fed knock at a
 * moved-on epoch simply fails to apply, and one at the same epoch is deduped
 * by the quarantine — so a failed write costs nothing worse than a re-read.
 */

const KEY_PREFIX = "lakko.ds.knock-cursor.v1:";

/** The last knock id this device consumed for `groupIdB64`, `0` when none. */
export function readKnockCursor(groupIdB64: string): number {
  try {
    const raw = globalThis.localStorage?.getItem(`${KEY_PREFIX}${groupIdB64}`);
    const n = raw === null || raw === undefined ? 0 : Number(raw);
    return Number.isFinite(n) && n >= 0 ? n : 0;
  } catch {
    return 0;
  }
}

/** Record that knocks up to `id` were consumed. Never moves backward. */
export function writeKnockCursor(groupIdB64: string, id: number): void {
  if (!Number.isFinite(id) || id < 0) return;
  if (id <= readKnockCursor(groupIdB64)) return;
  try {
    globalThis.localStorage?.setItem(`${KEY_PREFIX}${groupIdB64}`, String(id));
  } catch {
    /* storage unavailable: the next poll re-reads, which is harmless */
  }
}

/**
 * The floor on how often one device reads one group's knock queue: at most
 * once per this many milliseconds per group. A knock waits on a human
 * approval, so reading it faster buys nothing, and the knock read sits beside
 * the journal fetch, which returns early and often (a new row, a user
 * action, a wake) - iOS was measured reading knocks about twice a second
 * before this floor, which Android and web now share.
 */
export const KNOCK_READ_MIN_INTERVAL_MS = 10_000;

const lastKnockReadMs = new Map<string, number>();

/**
 * Is a knock read of `groupIdB64` due at `nowMs`? When it is, the read is
 * recorded as taken (the caller is about to make it), so a second caller in
 * the same window is told no. A clock that went backwards reads as due, so a
 * wrong clock can never stop knock reads for good.
 */
export function takeKnockReadSlot(groupIdB64: string, nowMs: number): boolean {
  const last = lastKnockReadMs.get(groupIdB64);
  if (last !== undefined && nowMs >= last && nowMs - last < KNOCK_READ_MIN_INTERVAL_MS) {
    return false;
  }
  lastKnockReadMs.set(groupIdB64, nowMs);
  return true;
}

/** Test hook: forget every recorded read. */
export function resetKnockReadSlotsForTests(): void {
  lastKnockReadMs.clear();
}
