// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 decision A — "this device founded the account's DeviceGroup alone"
 * (no other device answered its rejoin knock, or none was listed), kept
 * until every channel the account owns (the channel-subscription register's
 * Owner entries) has been founded again here or was already held here. Never
 * cleared at the first conversation: a restored phone whose chats came back
 * first must still re-found its channels, or the owner stays a reader with
 * nobody left to admit it (G-54). Per account, in `localStorage` (the bare
 * flag, no identifier besides the account id in the key); removed by the
 * account wipe.
 */

const PREFIX = "lakko.channels.founded-alone.v1.";

/** Where the marker lives (tests pass their own). */
export interface FoundedAloneStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function store(storage?: FoundedAloneStorage | null): FoundedAloneStorage | null {
  if (storage !== undefined) return storage;
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/** The DeviceGroup was founded here alone. */
export function markFoundedAlone(accountIdB64: string, storage?: FoundedAloneStorage | null): void {
  try {
    store(storage)?.setItem(PREFIX + accountIdB64, "1");
  } catch {
    /* storage unavailable: no re-found */
  }
}

/** Whether the marker is held. */
export function isFoundedAlone(accountIdB64: string, storage?: FoundedAloneStorage | null): boolean {
  try {
    return store(storage)?.getItem(PREFIX + accountIdB64) === "1";
  } catch {
    return false;
  }
}

/** Every owned channel was re-founded or skipped: the marker goes. */
export function clearFoundedAlone(accountIdB64: string, storage?: FoundedAloneStorage | null): void {
  try {
    store(storage)?.removeItem(PREFIX + accountIdB64);
  } catch {
    /* nothing to clear */
  }
}

/**
 * Pure: whether the marker's work is done. The register's live Owner
 * channels are each held here now (founded again or already held), or
 * there are none. A register not loaded yet (`null`) is never done.
 */
export function foundedAloneDone(
  ownedChannelIds: readonly string[] | null,
  holds: (channelIdHex: string) => boolean,
): boolean {
  if (ownedChannelIds === null) return false;
  return ownedChannelIds.every((id) => holds(id));
}
