// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057, the second security pass (finding 4): the per-(account, group)
 * "root held" marker.
 *
 * Owning a group in its role table and holding its journal root on the
 * Delivery Service are two different facts while an ownership transfer is in
 * flight: the role table moves at the commit, the root only when the new
 * owner's takeover (a `root` statement signed by the old root) lands. Until
 * then the DS refuses anything the new owner's root signs, so a new owner
 * that signed with it at once froze the group's membership.
 *
 * So an owner signs with its root only once this marker is set; before that
 * it keeps using the registrar seed the old owner appointed it with. The
 * marker is set when the root is known to be this account's:
 *
 *   - the creation claim, or a lazy claim, answered 204;
 *   - the takeover's `root` statement answered 204;
 *   - the probe (`root` naming this account's own root, signed by it)
 *     answered 204 - which is also how a second device of the new owner, or
 *     an owner device that predates the marker, learns it.
 *
 * Group ids only, nothing secret: kept in `localStorage`.
 */

const PREFIX = "lakko/ds-root-held/v1/";

/** The storage the marker uses; injectable for tests. */
export interface RootHeldStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem?(key: string): void;
}

function defaultStorage(): RootHeldStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

function load(accountId: string, storage: RootHeldStorage | null): Set<string> {
  const out = new Set<string>();
  if (storage === null) return out;
  try {
    const raw = storage.getItem(PREFIX + accountId);
    if (raw === null) return out;
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return out;
    for (const v of parsed) if (typeof v === "string") out.add(v);
  } catch {
    /* unreadable: nothing held; the probe sets it again */
  }
  return out;
}

/** `true` when this account is known to hold the group's journal root. */
export function isRootHeld(
  accountId: string,
  groupIdB64: string,
  storage: RootHeldStorage | null = defaultStorage(),
): boolean {
  return load(accountId, storage).has(groupIdB64);
}

/** Set the marker. Best effort. */
export function markRootHeld(
  accountId: string,
  groupIdB64: string,
  storage: RootHeldStorage | null = defaultStorage(),
): void {
  if (storage === null) return;
  const held = load(accountId, storage);
  if (held.has(groupIdB64)) return;
  held.add(groupIdB64);
  try {
    storage.setItem(PREFIX + accountId, JSON.stringify([...held].sort()));
  } catch {
    /* best effort: a lost marker costs one probe */
  }
}

/** Clear the marker (this account handed the root over). */
export function clearRootHeld(
  accountId: string,
  groupIdB64: string,
  storage: RootHeldStorage | null = defaultStorage(),
): void {
  if (storage === null) return;
  const held = load(accountId, storage);
  if (!held.delete(groupIdB64)) return;
  try {
    storage.setItem(PREFIX + accountId, JSON.stringify([...held].sort()));
  } catch {
    /* best effort */
  }
}

/** Forget one account's markers (forget device). */
export function clearRootHeldAll(accountId: string, storage: RootHeldStorage | null = defaultStorage()): void {
  try {
    storage?.removeItem?.(PREFIX + accountId);
  } catch {
    /* best effort */
  }
}
