// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "This device was just restored and its chats have not come back yet", per
 * account (`docs/ux/identity-recovery.md` section 7.2).
 *
 * Set when a restore from the phrase or a passkey finished, with how many
 * conversations the backup named. The chat list reads it for its card and its
 * banner, and clears it for good once the first conversation arrives. A
 * restored device that knew no conversation has nothing coming back, so
 * nothing is recorded and it shows the ordinary empty list.
 *
 * `localStorage`, keyed by the account; a small count, no secret. Forget
 * device and Delete account drop it (`auth/wipe-account.ts`).
 */
import { createSignal, type Accessor } from "solid-js";

const PREFIX = "lakko.backup.restored-marker.v1.";

/** The slice of `Storage` this module uses; injectable for tests. */
export interface RestoredMarkerStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function defaultStorage(): RestoredMarkerStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

const [version, setVersion] = createSignal(0);

/** Reactive: moves whenever a marker is written or dropped. */
export const restoredMarkerVersion: Accessor<number> = version;

/** A restore finished for `accountIdB64`; the backup named `conversations`. */
export function markRestoreFinished(
  accountIdB64: string,
  conversations: number,
  storage: RestoredMarkerStorage | null = defaultStorage(),
): void {
  if (storage === null || accountIdB64.length === 0 || conversations <= 0) return;
  try {
    storage.setItem(PREFIX + accountIdB64, "1");
    setVersion((v) => v + 1);
  } catch {
    // Storage blocked: the list is the ordinary empty one.
  }
}

/** Whether the list still owes this account its explanation. */
export function isRestoreMarked(
  accountIdB64: string,
  storage: RestoredMarkerStorage | null = defaultStorage(),
): boolean {
  if (storage === null || accountIdB64.length === 0) return false;
  try {
    return storage.getItem(PREFIX + accountIdB64) !== null;
  } catch {
    return false;
  }
}

/** The first conversation arrived, or the account is being wiped. */
export function clearRestoreMarker(
  accountIdB64: string,
  storage: RestoredMarkerStorage | null = defaultStorage(),
): void {
  if (storage === null) return;
  try {
    if (storage.getItem(PREFIX + accountIdB64) === null) return;
    storage.removeItem(PREFIX + accountIdB64);
    setVersion((v) => v + 1);
  } catch {
    // Nothing to clear.
  }
}

/** What the chat list shows for a restored device. */
export type RestoredListState = "none" | "card" | "banner";

/**
 * `docs/ux/identity-recovery.md` section 7.2. `otherDeviceSignedIn` is `null`
 * while it is not known yet: nothing is claimed until it is. The card stands
 * only on the empty list; the banner above a list that is still filling.
 */
export function restoredListState(facts: {
  readonly marked: boolean;
  readonly conversationCount: number;
  readonly otherDeviceSignedIn: boolean | null;
}): RestoredListState {
  if (!facts.marked || facts.conversationCount > 0 || facts.otherDeviceSignedIn === null) return "none";
  return facts.otherDeviceSignedIn ? "banner" : "card";
}
