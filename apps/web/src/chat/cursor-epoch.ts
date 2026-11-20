// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 phase 1b - the MLS epoch a conversation's journal cursor was
 * advanced at.
 *
 * ## What it catches
 *
 * The poll persists the group snapshot and only then advances the cursor
 * (C-67), so the cursor never leads the snapshot that was written with it.
 * But a LATER write can put an older snapshot back (a stale handle of the
 * same group writing what it holds, a restore of an older provider). The
 * cursor is then past the commit that leaves the loaded epoch: that commit is
 * never read again, and the device is stranded exactly as if it had skipped
 * it (holding a row cannot catch this - the cursor is already past).
 *
 * Recording, with each cursor advance, the epoch the group was at when its
 * snapshot reached disk makes the case visible: a group loaded at an epoch
 * below the recorded one has been rolled back under its cursor. The poll then
 * reads again from the commit the DS registered as leaving the loaded epoch
 * (`FetchResult.registeredCommits`, ADR-0091 open question 6) instead of from
 * the cursor; see {@link snapshotRewindFrom}.
 *
 * ## The record
 *
 * One `localStorage` row per (account, group):
 * `lakko.chat.cursor-epoch.v1:<account>:<group>`, the highest epoch recorded
 * (it never moves backward: a recorded epoch above the loaded one stays until
 * the group is back at or above it, so a rewind that takes several polls is
 * not forgotten half way). The row carries an epoch number only. Dropped with
 * the group (`membership-wipe.ts`) and on Forget device (`wipe-account.ts`);
 * the writes go through the store gate on `Storage.setItem` (ADR-0077).
 */

import { boundContactsAccount } from "../contacts/local-store";

const KEY_PREFIX = "lakko.chat.cursor-epoch.v1:";

function accountOf(owner: string | undefined): string | null {
  const account = owner ?? boundContactsAccount();
  return account === null || account.length === 0 ? null : account;
}

function keyFor(account: string, groupIdB64: string): string {
  return `${KEY_PREFIX}${account}:${groupIdB64}`;
}

/** The epoch recorded with `groupIdB64`'s cursor, `undefined` when none (or
 *  with no account, or unreadable). */
export function readCursorEpoch(groupIdB64: string, owner?: string): bigint | undefined {
  const account = accountOf(owner);
  if (account === null) return undefined;
  try {
    const raw = globalThis.localStorage?.getItem(keyFor(account, groupIdB64));
    if (raw === null || raw === undefined || !/^\d{1,20}$/.test(raw)) return undefined;
    return BigInt(raw);
  } catch {
    return undefined;
  }
}

/** Record that the cursor advanced while the durable snapshot was at
 *  `epoch`. Never moves backward; a no-op with no account. */
export function recordCursorEpoch(groupIdB64: string, epoch: bigint, owner?: string): void {
  const account = accountOf(owner);
  if (account === null || epoch < 0n) return;
  const stored = readCursorEpoch(groupIdB64, account);
  if (stored !== undefined && stored >= epoch) return;
  try {
    globalThis.localStorage?.setItem(keyFor(account, groupIdB64), epoch.toString());
  } catch {
    /* storage unavailable or the session sealed: a missing record only
       disables the rewind check for this group */
  }
}

/** Drop one group's record (the group left this device). */
export function clearCursorEpochForGroup(groupIdB64: string, owner?: string): void {
  const account = accountOf(owner);
  if (account === null) return;
  try {
    globalThis.localStorage?.removeItem(keyFor(account, groupIdB64));
  } catch {
    /* nothing to clear */
  }
}

/** Forget device: drop every record of `accountIdB64`. */
export function clearCursorEpochsForAccount(accountIdB64: string): void {
  try {
    const store = globalThis.localStorage;
    if (store === undefined) return;
    const own = `${KEY_PREFIX}${accountIdB64}:`;
    const victims: string[] = [];
    for (let i = 0; i < store.length; i += 1) {
      const k = store.key(i);
      if (k !== null && k.startsWith(own)) victims.push(k);
    }
    for (const k of victims) store.removeItem(k);
  } catch {
    /* nothing to clear */
  }
}

/**
 * Where a poll must read from instead of `after`, or `undefined` when it
 * reads from `after` as usual.
 *
 * A rewind is due exactly when the group was loaded below the epoch recorded
 * with its cursor (`loaded < recorded`) and the DS names the commit leaving
 * the loaded epoch at or below the cursor (`leaving <= after`): that commit
 * was read before the snapshot went back, and nothing after the cursor can
 * bring the group forward. The read restarts just before it. A commit the DS
 * no longer names (pruned after its retention, a Replace) cannot be read
 * again: no rewind, and the device is behind (ADR-0091 phase 2).
 */
export function snapshotRewindFrom(input: {
  readonly after: number;
  readonly loaded: bigint | undefined;
  readonly recorded: bigint | undefined;
  readonly registeredCommits: ReadonlyMap<bigint, number> | undefined;
}): number | undefined {
  const { after, loaded, recorded, registeredCommits } = input;
  if (loaded === undefined || recorded === undefined || loaded >= recorded) return undefined;
  const leaving = registeredCommits?.get(loaded + 1n);
  if (leaving === undefined || leaving > after || leaving < 1) return undefined;
  return leaving - 1;
}

/** The line logged on a rewind (no identifier). */
export function rewindLine(loaded: bigint, recorded: bigint): string {
  return (
    `chat snapshot behind its cursor: loaded=${loaded} recorded=${recorded}; ` +
    "reading again from the commit that leaves the loaded epoch"
  );
}
