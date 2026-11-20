// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The group ids a New group attempt claimed an `@name` with, kept per account
 * until the group exists (ADR-0087 section 3, note of 2026-11-14).
 *
 * A group's `@name` is claimed before anything about the group leaves the
 * device, under a write key derived from the group id. A step after the claim
 * can fail, or the claim's answer can be lost, and every attempt founds a new
 * group with a new random id, so a retry is another key and discovery answers
 * its own name `name-taken`. The ids of the earlier attempts are kept here
 * (newest first) so a retry can move the name to its new group: a `rekey`
 * signed by the earlier group's key, then an update under the new key.
 *
 * `localStorage`, keyed by the account; `@name`s and group ids of groups this
 * account is creating, sent to discovery only as OPRF lookups and signatures.
 * Forget device and Delete account drop it (`auth/wipe-account.ts`).
 */
import { HandleWriteError } from "../discovery/handle-lookup";

const PREFIX = "lakko.groups.claimed-handles.v1.";

/** How many attempts are kept per name. Chosen, not measured: each retry
 *  tries the kept ids newest first, one request each. */
export const KEPT_GROUP_ATTEMPTS = 8;

/** The slice of `Storage` this module uses; injectable for tests. */
export interface ClaimedGroupHandleStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function defaultStorage(): ClaimedGroupHandleStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

function canonical(handle: string): string {
  return handle.trim().replace(/^@+/, "").toLowerCase();
}

function read(storage: ClaimedGroupHandleStorage | null, accountIdB64: string): Record<string, string[]> {
  try {
    const raw = storage?.getItem(PREFIX + accountIdB64) ?? null;
    if (raw === null) return {};
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return {};
    const out: Record<string, string[]> = {};
    for (const [k, v] of Object.entries(parsed)) {
      if (Array.isArray(v)) {
        const ids = v.filter((x): x is string => typeof x === "string");
        if (ids.length > 0) out[k] = ids;
      }
    }
    return out;
  } catch {
    return {};
  }
}

function write(
  storage: ClaimedGroupHandleStorage | null,
  accountIdB64: string,
  held: Readonly<Record<string, string[]>>,
): void {
  try {
    if (Object.keys(held).length === 0) storage?.removeItem(PREFIX + accountIdB64);
    else storage?.setItem(PREFIX + accountIdB64, JSON.stringify(held));
  } catch {
    // Storage blocked: a failed create burns the name, as it did.
  }
}

/** The group ids earlier attempts claimed `handle` with, newest first. */
export function keptGroupAttempts(
  accountIdB64: string,
  handle: string,
  storage: ClaimedGroupHandleStorage | null = defaultStorage(),
): string[] {
  return read(storage, accountIdB64)[canonical(handle)] ?? [];
}

/** Whether a create on this account claimed `handle` and has not finished:
 *  the name is this account's to take back, not "taken". */
export function isClaimedGroupHandle(
  accountIdB64: string,
  handle: string,
  storage: ClaimedGroupHandleStorage | null = defaultStorage(),
): boolean {
  return keptGroupAttempts(accountIdB64, handle, storage).length > 0;
}

/** Keep this attempt's group id for `handle`, before any request about the
 *  name goes out. Returns the earlier attempts, newest first. */
export function keepGroupAttempt(
  accountIdB64: string,
  handle: string,
  groupIdB64: string,
  storage: ClaimedGroupHandleStorage | null = defaultStorage(),
): string[] {
  const held = read(storage, accountIdB64);
  const key = canonical(handle);
  const earlier = (held[key] ?? []).filter((id) => id !== groupIdB64);
  write(storage, accountIdB64, { ...held, [key]: [groupIdB64, ...earlier].slice(0, KEPT_GROUP_ATTEMPTS) });
  return earlier;
}

/** The group exists, or the name turned out to be someone else's. */
export function forgetGroupAttempts(
  accountIdB64: string,
  handle: string,
  storage: ClaimedGroupHandleStorage | null = defaultStorage(),
): void {
  const held = read(storage, accountIdB64);
  const key = canonical(handle);
  if (held[key] === undefined) return;
  const { [key]: _gone, ...rest } = held;
  write(storage, accountIdB64, rest);
}

/** Forget device, Delete account. */
export function clearClaimedGroupHandles(
  accountIdB64: string,
  storage: ClaimedGroupHandleStorage | null = defaultStorage(),
): void {
  try {
    storage?.removeItem(PREFIX + accountIdB64);
  } catch {
    // Nothing to clear.
  }
}

/**
 * Take `handle` for the group this attempt founded.
 *
 * With no earlier attempt: a plain claim. Otherwise, for each earlier
 * attempt newest first, move the name from that attempt's key to this one's
 * (`rekeyFrom`) and then write this group's content under the new key
 * (`update`):
 * - `404` (no row for the name: no earlier claim landed): a plain claim;
 * - `401` (the row is another key's): the next earlier attempt;
 * - every earlier attempt refused: the name is someone else's,
 *   `409 name-taken`.
 */
export async function claimOrTakeBackGroupHandle<T>(
  earlier: readonly string[],
  ops: {
    readonly claim: () => Promise<T>;
    readonly rekeyFrom: (earlierGroupIdB64: string) => Promise<void>;
    readonly update: () => Promise<T>;
  },
): Promise<T> {
  if (earlier.length === 0) return ops.claim();
  for (const id of earlier) {
    try {
      await ops.rekeyFrom(id);
    } catch (err) {
      if (err instanceof HandleWriteError && err.status === 404) return ops.claim();
      if (err instanceof HandleWriteError && err.status === 401) continue;
      throw err;
    }
    return ops.update();
  }
  throw new HandleWriteError(409, "name-taken");
}
