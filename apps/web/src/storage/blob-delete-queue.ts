// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 section 6 — blobs this account wants gone from the server: the
 * files of a message deleted for everyone, an attachment uploaded and never
 * sent, a replaced or removed profile photo.
 *
 * A delete is tried at once; one that fails (offline, the server down) waits
 * here and is tried again at the next drain. The list is per account in
 * `localStorage` (blob ids only, no keys) and is wiped with the account. A
 * blob the server answers 403 for (not this account's, or the key does not
 * match) is dropped: trying again cannot help.
 */

const KEY_PREFIX = "lakko.blob-deletes-owed.v1.";
/** Bound on the list, oldest dropped first. Chosen. */
export const MAX_OWED_DELETES = 2000;

export interface QueueStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function defaultStorage(): QueueStorage | null {
  return typeof localStorage === "undefined" ? null : localStorage;
}

const HEX64 = /^[0-9a-f]{64}$/;

/** The owed ids of an account, oldest first. */
export function readOwedDeletes(accountId: string, storage: QueueStorage | null = defaultStorage()): string[] {
  try {
    const raw = storage?.getItem(KEY_PREFIX + accountId) ?? null;
    if (raw === null) return [];
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string" && HEX64.test(x)) : [];
  } catch {
    return [];
  }
}

function write(accountId: string, ids: readonly string[], storage: QueueStorage | null): void {
  try {
    if (ids.length === 0) storage?.removeItem(KEY_PREFIX + accountId);
    else storage?.setItem(KEY_PREFIX + accountId, JSON.stringify(ids));
  } catch {
    // Storage blocked: the delete is simply not retried.
  }
}

/** Add ids (deduplicated, bounded). */
export function owe(
  accountId: string,
  idsHex: readonly string[],
  storage: QueueStorage | null = defaultStorage(),
): string[] {
  const held = readOwedDeletes(accountId, storage);
  const seen = new Set(held);
  for (const id of idsHex) {
    const lower = id.toLowerCase();
    if (!HEX64.test(lower) || seen.has(lower)) continue;
    seen.add(lower);
    held.push(lower);
  }
  const bounded = held.length > MAX_OWED_DELETES ? held.slice(held.length - MAX_OWED_DELETES) : held;
  write(accountId, bounded, storage);
  return bounded;
}

/** What one delete attempt came to. */
export type DeleteOutcome = "deleted" | "refused" | "retry";

/**
 * Try every owed delete once, in order. `deleted` and `refused` leave the
 * list; `retry` stays. Re-reads the list at the end so ids owed meanwhile
 * are kept.
 */
export async function drainOwedDeletes(
  accountId: string,
  attempt: (idHex: string) => Promise<DeleteOutcome>,
  storage: QueueStorage | null = defaultStorage(),
): Promise<{ readonly done: number; readonly remaining: number }> {
  const ids = readOwedDeletes(accountId, storage);
  const gone = new Set<string>();
  for (const id of ids) {
    let outcome: DeleteOutcome;
    try {
      outcome = await attempt(id);
    } catch {
      outcome = "retry";
    }
    if (outcome !== "retry") gone.add(id);
  }
  const now = readOwedDeletes(accountId, storage).filter((id) => !gone.has(id));
  write(accountId, now, storage);
  return { done: gone.size, remaining: now.length };
}

/** Forget an account's list (wipe). */
export function clearOwedDeletes(accountId: string, storage: QueueStorage | null = defaultStorage()): void {
  write(accountId, [], storage);
}
