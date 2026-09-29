// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §2 trigger 3 — the growth baseline that survives a page session.
 *
 * `membersGrew` decides growth as a set difference against a previous roster,
 * and the chat view used to keep that previous roster in a plain variable: it
 * lived exactly as long as the view. A join that landed while this tab was
 * closed was therefore never growth to it — the first read after a reload was
 * a baseline — so the newcomer never received this device's name, and the
 * background receive, which polls every group the view does not, had no
 * baseline at all. The set is persisted here per (account, group) instead,
 * and BOTH receive paths fold their roster through {@link observeMembers}.
 *
 * The first observation of a group is still a baseline, not growth: this
 * device joining is trigger 1, and it sends a profile request of its own.
 *
 * Only PERSON keys are stored (base64 account keys, ADR-0050): a second phone
 * of an existing member is not a newcomer who lacks the names.
 */

import { membersGrew } from "./membership-growth";
import type { KeyValueStorage } from "./profile-request";

const STORE_PREFIX = "lakko.member-baseline.v1";

function storeKey(accountIdB64: string, groupIdB64: string): string {
  return `${STORE_PREFIX}.${accountIdB64}.${groupIdB64}`;
}

function defaultStorage(): KeyValueStorage | undefined {
  return (globalThis as { localStorage?: KeyValueStorage }).localStorage;
}

/** The persisted roster, or `null` when there is no baseline yet. Never
 *  throws: a corrupt row reads as "no baseline", which is the safe side —
 *  the next observation seeds it, and nobody is told the group grew. */
export function loadMemberBaseline(
  accountIdB64: string,
  groupIdB64: string,
  storage: KeyValueStorage | undefined = defaultStorage(),
): ReadonlySet<string> | null {
  try {
    const raw = storage?.getItem(storeKey(accountIdB64, groupIdB64));
    if (raw === null || raw === undefined) return null;
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return null;
    const out = new Set<string>();
    for (const k of parsed) if (typeof k === "string") out.add(k);
    return out;
  } catch {
    return null;
  }
}

function saveMemberBaseline(
  accountIdB64: string,
  groupIdB64: string,
  keys: ReadonlySet<string>,
  storage: KeyValueStorage | undefined,
): void {
  try {
    storage?.setItem(storeKey(accountIdB64, groupIdB64), JSON.stringify([...keys]));
  } catch {
    /* best-effort */
  }
}

/**
 * Fold one roster read into the persisted baseline. Returns whether the group
 * GAINED a member since the last observation (the `membersGrew` rule: a set
 * difference, never a count, never leaf indices).
 *
 * A torn read (an empty `current`) is neither growth nor a new baseline — it
 * leaves the store alone, so the next good read still compares against the
 * real previous roster. An unchanged roster writes nothing.
 */
export function observeMembers(
  accountIdB64: string,
  groupIdB64: string,
  current: ReadonlySet<string>,
  storage: KeyValueStorage | undefined = defaultStorage(),
): boolean {
  return observeMembership(accountIdB64, groupIdB64, current, storage).grew;
}

/** What one roster observation found against the persisted baseline. */
export interface MembershipChange {
  /** A person is present who was not (ADR-0048 trigger 3). */
  readonly grew: boolean;
  /** A person who was present is gone (ADR-0058 section 3: this account's
   *  `group` and `member` keys for the group rotate). Never on the first-ever
   *  observation, and never on an empty (torn) roster. */
  readonly shrank: boolean;
}

/** {@link observeMembers}, also reporting a shrink. Same baseline, same
 *  write: the two answers can never disagree on what was seen before. */
export function observeMembership(
  accountIdB64: string,
  groupIdB64: string,
  current: ReadonlySet<string>,
  storage: KeyValueStorage | undefined = defaultStorage(),
): MembershipChange {
  if (current.size === 0) return { grew: false, shrank: false };
  const previous = loadMemberBaseline(accountIdB64, groupIdB64, storage);
  const grew = membersGrew(previous, current);
  const shrank = previous !== null && [...previous].some((k) => !current.has(k));
  if (previous === null || !sameSet(previous, current)) {
    saveMemberBaseline(accountIdB64, groupIdB64, current, storage);
  }
  return { grew, shrank };
}

function sameSet(a: ReadonlySet<string>, b: ReadonlySet<string>): boolean {
  if (a.size !== b.size) return false;
  for (const k of a) if (!b.has(k)) return false;
  return true;
}
