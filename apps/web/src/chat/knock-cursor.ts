// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0053 contract point 7 — the position an owner / admin device has read a
 * group's KNOCK queue up to.
 *
 * Its own cursor, separate from the journal's: the knock queue is a different
 * DS table with its own id sequence, so a journal id means nothing in it.
 *
 * Persisted per account and group in `localStorage` rather than kept in memory, because
 * re-feeding every knock after a reload would resurface a request the admin
 * already REJECTED (a reject is a local discard of the retained proposal; the
 * knock row stays on the DS for its seven days, and at an unchanged epoch the
 * proposal would apply again). Lagging is harmless — a re-fed knock at a
 * moved-on epoch simply fails to apply, and one at the same epoch is deduped
 * by the quarantine — so a failed write costs nothing worse than a re-read.
 */

import { boundContactsAccount } from "../contacts/local-store";

/**
 * One row per (account, group): `lakko.ds.knock-cursor.v2:<account>:<group>`.
 *
 * The cursor is the reading ACCOUNT's progress, not the browser's: two
 * accounts that both administer one group each read its knock queue with
 * their own MLS state, and a request one of them rejected is not rejected by
 * the other. The v1 row was keyed by the group alone, so a second account
 * signed in on the same browser started where the first had stopped and
 * never saw the requests the first had consumed (2026-09-30).
 *
 * Which account a call means: the trailing `owner` argument, otherwise the
 * signed-in account bound through `bindContactsAccount`
 * (`contacts/local-store.ts`). With no account a read is `0` and a write is
 * a no-op. Log out keeps the rows; Forget device drops the account's
 * ({@link clearKnockCursorsForAccount}).
 */
const KEY_PREFIX = "lakko.ds.knock-cursor.v2:";
/** The retired device-wide rows (`<prefix><group>`). Never read; dropped by
 *  Forget device. */
const RETIRED_PREFIX = "lakko.ds.knock-cursor.v1:";

function accountOf(owner: string | undefined): string | null {
  const account = owner ?? boundContactsAccount();
  return account === null || account.length === 0 ? null : account;
}

function keyFor(account: string, groupIdB64: string): string {
  return `${KEY_PREFIX}${account}:${groupIdB64}`;
}

/** The last knock id `owner` (default: the signed-in account) consumed for
 *  `groupIdB64` on this browser, `0` when none or with no account. */
export function readKnockCursor(groupIdB64: string, owner?: string): number {
  const account = accountOf(owner);
  if (account === null) return 0;
  try {
    const raw = globalThis.localStorage?.getItem(keyFor(account, groupIdB64));
    const n = raw === null || raw === undefined ? 0 : Number(raw);
    return Number.isFinite(n) && n >= 0 ? n : 0;
  } catch {
    return 0;
  }
}

/** Record that knocks up to `id` were consumed. Never moves backward. A
 *  no-op with no account. */
export function writeKnockCursor(groupIdB64: string, id: number, owner?: string): void {
  const account = accountOf(owner);
  if (account === null) return;
  if (!Number.isFinite(id) || id < 0) return;
  if (id <= readKnockCursor(groupIdB64, account)) return;
  try {
    globalThis.localStorage?.setItem(keyFor(account, groupIdB64), String(id));
  } catch {
    /* storage unavailable: the next poll re-reads, which is harmless */
  }
}

/** Drop one group's knock cursor (ADR-0087 section 9). */
export function clearKnockCursorForGroup(groupIdB64: string, owner?: string): void {
  const account = accountOf(owner);
  if (account === null) return;
  try {
    globalThis.localStorage?.removeItem(keyFor(account, groupIdB64));
  } catch {
    /* storage unavailable: nothing to clear */
  }
}

/** Forget device: drop every knock cursor of `accountIdB64` and the retired
 *  device-wide rows. Another account's cursors are left alone. */
export function clearKnockCursorsForAccount(accountIdB64: string): void {
  try {
    const store = globalThis.localStorage;
    if (store === undefined) return;
    const own = `${KEY_PREFIX}${accountIdB64}:`;
    const victims: string[] = [];
    for (let i = 0; i < store.length; i += 1) {
      const k = store.key(i);
      if (k !== null && (k.startsWith(own) || k.startsWith(RETIRED_PREFIX))) victims.push(k);
    }
    for (const k of victims) store.removeItem(k);
  } catch {
    /* storage unavailable: nothing to clear */
  }
}

/**
 * The floor on how often one device reads one group's knock queue: at most
 * once per this many milliseconds per group. A knock waits on a human
 * approval, so reading it faster buys nothing, and the knock read sits beside
 * the journal fetch, which returns early and often (a new row, a user
 * action, a wake) - iOS was measured reading knocks about twice a second
 * before this floor, which Android and web now share. Raised from 10 s to
 * 60 s on 2026-11-05 (an idle web client read 13 knock queues in 20 s): a
 * knock is only read by an owner or admin, and a minute is nothing next to
 * the human approval it waits on.
 */
export const KNOCK_READ_MIN_INTERVAL_MS = 60_000;

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
