// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The channel id a handle was claimed for, kept per account until the channel
 * exists (D5).
 *
 * A handle is claimed at discovery before the channel exists on the channels
 * service. When that second step fails the handle stands claimed, and only
 * the id it was claimed with may claim it again (the same id is the same
 * handle write key, whose claim discovery answers as the first one), so the
 * id has to outlive the attempt: a fresh id on the retry read as "already
 * taken" for the person who took it, and the name stayed burned.
 *
 * `localStorage`, keyed by the account; handles and channel ids of channels
 * this account is creating, which it sends to discovery only as OPRF lookups.
 * Forget device and Delete account drop it (`auth/wipe-account.ts`).
 */

const PREFIX = "lakko.channels.claimed-handles.v1.";

/** The slice of `Storage` this module uses; injectable for tests. */
export interface ClaimedHandleStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function defaultStorage(): ClaimedHandleStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

function canonical(handle: string): string {
  return handle.trim().replace(/^@+/, "").toLowerCase();
}

function read(storage: ClaimedHandleStorage | null, accountIdB64: string): Record<string, string> {
  try {
    const raw = storage?.getItem(PREFIX + accountIdB64) ?? null;
    if (raw === null) return {};
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return {};
    return Object.fromEntries(
      Object.entries(parsed).filter((e): e is [string, string] => typeof e[1] === "string"),
    );
  } catch {
    return {};
  }
}

function write(
  storage: ClaimedHandleStorage | null,
  accountIdB64: string,
  held: Readonly<Record<string, string>>,
): void {
  try {
    if (Object.keys(held).length === 0) storage?.removeItem(PREFIX + accountIdB64);
    else storage?.setItem(PREFIX + accountIdB64, JSON.stringify(held));
  } catch {
    // Storage blocked: a failed create burns the handle, as it did.
  }
}

/** Whether a create on this account claimed `handle` and has not finished:
 *  the name is this account's to take back, not "taken". */
export function isClaimedHandle(
  accountIdB64: string,
  handle: string,
  storage: ClaimedHandleStorage | null = defaultStorage(),
): boolean {
  return read(storage, accountIdB64)[canonical(handle)] !== undefined;
}

/** The id to create a channel under `handle` with: the one an earlier attempt
 *  claimed it for, else `fresh`, which is recorded now, before the claim. */
export function channelIdForHandle(
  accountIdB64: string,
  handle: string,
  fresh: string,
  storage: ClaimedHandleStorage | null = defaultStorage(),
): string {
  const held = read(storage, accountIdB64);
  const key = canonical(handle);
  const kept = held[key];
  if (kept !== undefined) return kept;
  write(storage, accountIdB64, { ...held, [key]: fresh });
  return fresh;
}

/** The channel exists, or the handle turned out to be someone else's. */
export function forgetClaimedHandle(
  accountIdB64: string,
  handle: string,
  storage: ClaimedHandleStorage | null = defaultStorage(),
): void {
  const held = read(storage, accountIdB64);
  const key = canonical(handle);
  if (held[key] === undefined) return;
  const { [key]: _gone, ...rest } = held;
  write(storage, accountIdB64, rest);
}

/** Forget device, Delete account. */
export function clearClaimedHandles(
  accountIdB64: string,
  storage: ClaimedHandleStorage | null = defaultStorage(),
): void {
  try {
    storage?.removeItem(PREFIX + accountIdB64);
  } catch {
    // Nothing to clear.
  }
}
