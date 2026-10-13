// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which channels' owner key this account handed over to the key of its new
 * identity after a key rotation (ADR-0074 section 3.5, "Channels"). From
 * then on the channel's owner requests are signed by the NEW account
 * identity, while the channel's PublisherGroup stays in the provider of the
 * identity the account's conversation layer runs under.
 *
 * Plain storage: channel ids are not secret from this device. Dropped by the
 * account wipe together with the channel log.
 */

function key(accountIdB64: string): string {
  return `lakko:channel-owner-moved:${accountIdB64}`;
}

export function loadOwnerMoved(accountIdB64: string, storage: Storage | undefined = defaultStorage()): Set<string> {
  try {
    const raw = storage?.getItem(key(accountIdB64)) ?? null;
    if (raw === null) return new Set();
    const parsed: unknown = JSON.parse(raw);
    return new Set(
      Array.isArray(parsed) ? parsed.filter((v): v is string => typeof v === "string" && /^[0-9a-f]+$/.test(v)) : [],
    );
  } catch {
    return new Set();
  }
}

export function markOwnerMoved(
  accountIdB64: string,
  channelIdHex: string,
  storage: Storage | undefined = defaultStorage(),
): void {
  try {
    const moved = loadOwnerMoved(accountIdB64, storage);
    moved.add(channelIdHex.toLowerCase());
    storage?.setItem(key(accountIdB64), JSON.stringify([...moved]));
  } catch {
    // Storage blocked: the handover is asked again (it answers 204).
  }
}

export function clearOwnerMoved(accountIdB64: string, storage: Storage | undefined = defaultStorage()): void {
  try {
    storage?.removeItem(key(accountIdB64));
  } catch {
    /* blocked */
  }
}

function defaultStorage(): Storage | undefined {
  return typeof localStorage === "undefined" ? undefined : localStorage;
}
