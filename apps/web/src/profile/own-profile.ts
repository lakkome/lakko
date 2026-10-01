// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 — this device's record of the viewer's OWN profile name.
 *
 * `{ displayName, updatedAtMs }` per account, in `localStorage`. The username
 * is not stored here: it is the one the account registered and lives with the
 * session. `updatedAtMs` is stamped at save time and travels in every
 * published envelope, so the receivers' strictly-newer rule orders this
 * device's changes against another device's.
 *
 * A device that never saved a name publishes `updatedAtMs = 0`, which loses
 * to any saved name from another device of the same account — the right
 * direction, since it knows nothing newer.
 */

const KEY_PREFIX = "lakko.profile.own.v1.";

/** The own profile as persisted. */
export interface OwnProfile {
  /** `null` for "no name". */
  readonly displayName: string | null;
  /** Local clock at the last save; `0` when never saved. */
  readonly updatedAtMs: number;
}

/** The profile of an account that never saved one. */
export const EMPTY_OWN_PROFILE: OwnProfile = { displayName: null, updatedAtMs: 0 };

/** The subset of `Storage` this module uses. */
export interface OwnProfileStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function defaultStorage(): OwnProfileStorage | null {
  return typeof localStorage === "undefined" ? null : localStorage;
}

/** Read an account's own profile; the empty profile when none or malformed. */
export function loadOwnProfile(
  accountId: string,
  storage: OwnProfileStorage | null = defaultStorage(),
): OwnProfile {
  const raw = storage?.getItem(KEY_PREFIX + accountId) ?? null;
  if (raw === null) return EMPTY_OWN_PROFILE;
  try {
    const parsed = JSON.parse(raw) as { displayName?: unknown; updatedAtMs?: unknown };
    const name = parsed.displayName;
    const at = parsed.updatedAtMs;
    if ((name !== null && typeof name !== "string") || typeof at !== "number" || !Number.isFinite(at)) {
      return EMPTY_OWN_PROFILE;
    }
    return { displayName: name === "" ? null : name, updatedAtMs: Math.max(0, Math.floor(at)) };
  } catch {
    return EMPTY_OWN_PROFILE;
  }
}

/**
 * Save an already-validated display name (the caller ran the core's
 * `normalizeDisplayName`). The stamp never moves backwards: a save on a clock
 * that jumped back still produces a strictly newer stamp than the last one, so
 * receivers do not silently ignore it.
 */
export function saveOwnProfile(
  accountId: string,
  displayName: string | null,
  nowMs: number,
  storage: OwnProfileStorage | null = defaultStorage(),
): OwnProfile {
  const previous = loadOwnProfile(accountId, storage);
  const updatedAtMs = Math.max(Math.floor(nowMs), previous.updatedAtMs + 1);
  const next: OwnProfile = { displayName, updatedAtMs };
  storage?.setItem(KEY_PREFIX + accountId, JSON.stringify(next));
  return next;
}

/**
 * ADR-0048 section 4a — adopt the own name another device of this account
 * sent over the DeviceGroup. Written only when `updatedAtMs` is strictly
 * greater than the stored stamp, and the stamp is kept exactly as received
 * (never re-stamped), so a later save here still orders after it. Returns
 * the stored profile when it was adopted, `null` otherwise. A stamp beyond
 * what a JS number holds exactly is refused.
 */
export function adoptOwnProfile(
  accountId: string,
  displayName: string | null,
  updatedAtMs: bigint,
  storage: OwnProfileStorage | null = defaultStorage(),
): OwnProfile | null {
  if (storage === null) return null;
  if (updatedAtMs <= 0n || updatedAtMs > BigInt(Number.MAX_SAFE_INTEGER)) return null;
  const previous = loadOwnProfile(accountId, storage);
  if (updatedAtMs <= BigInt(previous.updatedAtMs)) return null;
  const next: OwnProfile = {
    displayName: displayName === "" ? null : displayName,
    updatedAtMs: Number(updatedAtMs),
  };
  storage.setItem(KEY_PREFIX + accountId, JSON.stringify(next));
  return next;
}

/** Drop an account's own profile (forget device). */
export function clearOwnProfile(
  accountId: string,
  storage: OwnProfileStorage | null = defaultStorage(),
): void {
  storage?.removeItem(KEY_PREFIX + accountId);
}
