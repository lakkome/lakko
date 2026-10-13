// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 phase 2 (UX 16.6) — the channels this device owned under account
 * keys that were since reset.
 *
 * A channel's owner proof key derives from the account seed and its content
 * seed from a PublisherGroup in the account identity's provider, so a reset
 * orphans every channel the person owned: no device can post to it, rotate
 * it or close it any more, including the one that reset the keys. The
 * device that reset them is the ONLY place that knows which channels those
 * were, and it learns it exactly once: at the first channel-log hydration
 * under the new identity, where the stored provider snapshot belongs to the
 * previous identity and the publisher signing seeds it carries name the
 * channels (`state/channels-state.ts`). That conclusion does not survive the
 * next flush — the new identity's provider replaces the old snapshot and the
 * keys that were never re-opened drop out of the persisted seeds — so it is
 * recorded here, per account, and only the person's own `Remove from this
 * device` takes a channel out of it.
 *
 * Nothing is sent anywhere: a subscriber has no signal (16.6, 16.10).
 */

const PREFIX = "lakko.channels.orphaned-by-reset.v1";

function key(accountIdB64: string): string {
  return `${PREFIX}.${accountIdB64}`;
}

/** The orphaned channel ids (hex) of an account. Empty on any failure. */
export function loadOrphanedByReset(accountIdB64: string): Set<string> {
  try {
    const raw = globalThis.localStorage?.getItem(key(accountIdB64));
    if (raw === null || raw === undefined) return new Set();
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return new Set();
    return new Set(parsed.filter((v): v is string => typeof v === "string"));
  } catch {
    return new Set();
  }
}

/** Add ids (a hydration found them under the previous identity). Returns
 *  the resulting set. Never throws. */
export function recordOrphanedByReset(accountIdB64: string, ids: Iterable<string>): Set<string> {
  const next = loadOrphanedByReset(accountIdB64);
  for (const id of ids) next.add(id);
  save(accountIdB64, next);
  return next;
}

/** `Remove from this device` took one channel out. Returns the set. */
export function forgetOrphanedByReset(accountIdB64: string, id: string): Set<string> {
  const next = loadOrphanedByReset(accountIdB64);
  next.delete(id);
  save(accountIdB64, next);
  return next;
}

/** `forgetDevice` hygiene. */
export function clearOrphanedByResetForAccount(accountIdB64: string): void {
  try {
    globalThis.localStorage?.removeItem(key(accountIdB64));
  } catch {
    /* best-effort */
  }
}

function save(accountIdB64: string, ids: ReadonlySet<string>): void {
  try {
    if (ids.size === 0) globalThis.localStorage?.removeItem(key(accountIdB64));
    else globalThis.localStorage?.setItem(key(accountIdB64), JSON.stringify([...ids]));
  } catch {
    /* best-effort: the in-memory set stands for the session */
  }
}
