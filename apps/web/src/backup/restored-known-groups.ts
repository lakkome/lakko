// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The conversations a restored backup named that this device has not joined
 * yet ("restored known groups"), per account.
 *
 * The account backup carries no MLS state (2026-10-02): a restore learns
 * WHICH conversations the account has, never the means to open them. The
 * device is admitted to each by the other members later (ADR-0050 section
 * 2.5a). Until that happens the device holds none of those groups itself, so
 * a backup composed only from the groups it holds would upload an empty list
 * and erase the account's record of its conversations at the first upload
 * after a restore. This store closes that: every backup this device uploads
 * lists the groups it holds PLUS the restored ones it has not joined yet
 * ({@link backupKnownGroupIds}).
 *
 * An id leaves the set when the device joins that group (it is then one of
 * the groups the device holds), when the user deletes that chat
 * ({@link dropRestoredKnownGroup} from `chat/delete-chat.ts`), or when the
 * local group has ended - an ended group is still held until it is deleted,
 * so the join rule has already dropped it.
 *
 * This is NOT the pairing bundle's list (`devices/state-bundle.ts`,
 * `lakko.pairing.known-groups.v2.`) and never feeds the chat list's
 * "Synchronizing..." label: a device restored with no other device alive has
 * nobody who is about to admit it, so there is nothing to announce.
 *
 * `localStorage`, keyed by the account; group ids only. Log out keeps the
 * row, Forget device and Delete account drop it (`auth/wipe-account.ts`).
 */

/** Prefix of the per-account row; the account id (standard base64) follows. */
const RESTORED_GROUPS_PREFIX = "lakko.backup.restored-groups.v1.";

/** The slice of `Storage` this module uses; injectable for tests. */
export interface RestoredGroupsStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function defaultStorage(): RestoredGroupsStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

function write(
  accountIdB64: string,
  ids: readonly string[],
  storage: RestoredGroupsStorage | null,
): void {
  if (storage === null) return;
  try {
    if (ids.length === 0) storage.removeItem(RESTORED_GROUPS_PREFIX + accountIdB64);
    else storage.setItem(RESTORED_GROUPS_PREFIX + accountIdB64, JSON.stringify(ids));
  } catch {
    // Storage unavailable: the next upload lists only the groups this device
    // holds, which is what it did before this store existed.
  }
}

/** The restored group ids `accountIdB64` has not joined yet, base64. */
export function loadRestoredKnownGroups(
  accountIdB64: string,
  storage: RestoredGroupsStorage | null = defaultStorage(),
): string[] {
  if (storage === null || accountIdB64.length === 0) return [];
  try {
    const raw = storage.getItem(RESTORED_GROUPS_PREFIX + accountIdB64);
    if (raw === null) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return [...new Set(parsed.filter((x): x is string => typeof x === "string" && x.length > 0))];
  } catch {
    return [];
  }
}

/**
 * Record the group ids a restored backup carried. A union with what is
 * already stored, never a replacement: a second restore (a retry, or the
 * restore screen run again) must not forget what the first one learned.
 */
export function addRestoredKnownGroups(
  accountIdB64: string,
  idsB64: readonly string[],
  storage: RestoredGroupsStorage | null = defaultStorage(),
): void {
  if (accountIdB64.length === 0 || idsB64.length === 0) return;
  const next = new Set(loadRestoredKnownGroups(accountIdB64, storage));
  const before = next.size;
  for (const id of idsB64) if (id.length > 0) next.add(id);
  if (next.size === before) return;
  write(accountIdB64, [...next], storage);
}

/** Drop one id: the user deleted that chat, or the device joined it. */
export function dropRestoredKnownGroup(
  accountIdB64: string,
  groupIdB64: string,
  storage: RestoredGroupsStorage | null = defaultStorage(),
): void {
  const current = loadRestoredKnownGroups(accountIdB64, storage);
  if (!current.includes(groupIdB64)) return;
  write(
    accountIdB64,
    current.filter((id) => id !== groupIdB64),
    storage,
  );
}

/**
 * Drop every restored id the device now holds itself (`heldIdsB64`, the chat
 * registry's groups, ended ones included). Returns the ids still not joined.
 */
export function pruneJoinedRestoredGroups(
  accountIdB64: string,
  heldIdsB64: readonly string[],
  storage: RestoredGroupsStorage | null = defaultStorage(),
): string[] {
  const current = loadRestoredKnownGroups(accountIdB64, storage);
  if (current.length === 0) return [];
  const held = new Set(heldIdsB64);
  const pending = current.filter((id) => !held.has(id));
  if (pending.length !== current.length) write(accountIdB64, pending, storage);
  return pending;
}

/**
 * The conversation ids a backup of `accountIdB64` lists: the groups this
 * device holds, then the restored ones it has not joined yet. Prunes the
 * joined ones from the store as it goes, so the set cannot outlive its cause.
 */
export function backupKnownGroupIds(
  accountIdB64: string,
  heldIdsB64: readonly string[],
  storage: RestoredGroupsStorage | null = defaultStorage(),
): string[] {
  const pending = pruneJoinedRestoredGroups(accountIdB64, heldIdsB64, storage);
  return [...new Set([...heldIdsB64, ...pending])];
}

/** Forget device / Delete account: drop `accountIdB64`'s row. */
export function clearRestoredKnownGroups(
  accountIdB64: string,
  storage: RestoredGroupsStorage | null = defaultStorage(),
): void {
  if (storage === null) return;
  try {
    storage.removeItem(RESTORED_GROUPS_PREFIX + accountIdB64);
  } catch {
    // Nothing to clear.
  }
}
