// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "This device was just restored and its chats have not come back yet", per
 * account (`docs/ux/identity-recovery.md` section 7.2).
 *
 * Set when a restore from the phrase or a passkey finished, with how many
 * conversations the backup named. The chat list reads it for its card, and
 * clears it for good once the first conversation arrives. A
 * restored device that knew no conversation has nothing coming back, so
 * nothing is recorded and it shows the ordinary empty list.
 *
 * Beside it, the answer to this device's DeviceGroup rejoin (ADR-0081 third
 * amendment): whether another device of the account answered the knock, or
 * this device founded the DeviceGroup because nobody did (or because no other
 * device was listed). That answer, not whether another device is merely listed
 * in the directory, decides whether the card shows: a lost device stays
 * listed. It is
 * recorded whenever the device's DeviceGroup membership is decided, kept
 * across restarts, and dropped with the marker.
 *
 * `localStorage`, keyed by the account; a small count and one word, no
 * secret. Forget device and Delete account drop both (`auth/wipe-account.ts`).
 */
import { createSignal, type Accessor } from "solid-js";

import type { RejoinOutcome } from "../devices/device-group-rejoin";

const PREFIX = "lakko.backup.restored-marker.v1.";
const ANSWER_PREFIX = "lakko.backup.restore-answer.v1.";

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
    storage.setItem(PREFIX + accountIdB64, String(Math.floor(conversations)));
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

/** How many conversations the backup named for `accountIdB64`; 0 without a
 *  marker (or a marker this build cannot read). */
export function restoredConversationCount(
  accountIdB64: string,
  storage: RestoredMarkerStorage | null = defaultStorage(),
): number {
  if (storage === null || accountIdB64.length === 0) return 0;
  try {
    const raw = storage.getItem(PREFIX + accountIdB64);
    if (raw === null) return 0;
    const n = Number.parseInt(raw, 10);
    return Number.isFinite(n) && n > 0 ? n : 0;
  } catch {
    return 0;
  }
}

/** The first conversation arrived, or the account is being wiped. Drops the
 *  rejoin answer with the marker. */
export function clearRestoreMarker(
  accountIdB64: string,
  storage: RestoredMarkerStorage | null = defaultStorage(),
): void {
  if (storage === null) return;
  try {
    let changed = false;
    for (const key of [PREFIX + accountIdB64, ANSWER_PREFIX + accountIdB64]) {
      if (storage.getItem(key) === null) continue;
      storage.removeItem(key);
      changed = true;
    }
    if (changed) setVersion((v) => v + 1);
  } catch {
    // Nothing to clear.
  }
}

/**
 * The answer to this device's DeviceGroup rejoin: `answered` when another
 * device of the account answered the knock (a presence note or a Welcome),
 * `founded` when this device founded the DeviceGroup because nobody answered
 * or no other device was listed.
 */
export type RestoreAnswer = "answered" | "founded";

/** The answer a rejoin pass that ended in `outcome` gives. `waiting` means a
 *  device answered and its Welcome has not come yet; `no-handle` (no knock
 *  was made) answers nothing. */
export function answerForRejoinOutcome(outcome: RejoinOutcome): RestoreAnswer | null {
  if (outcome === "no-handle") return null;
  return outcome === "founded" ? "founded" : "answered";
}

/** Record the latest answer for `accountIdB64`. */
export function recordRestoreAnswer(
  accountIdB64: string,
  answer: RestoreAnswer,
  storage: RestoredMarkerStorage | null = defaultStorage(),
): void {
  if (storage === null || accountIdB64.length === 0) return;
  try {
    if (storage.getItem(ANSWER_PREFIX + accountIdB64) === answer) return;
    storage.setItem(ANSWER_PREFIX + accountIdB64, answer);
    setVersion((v) => v + 1);
  } catch {
    // Storage blocked: the list claims nothing.
  }
}

/** The recorded answer for `accountIdB64`; `null` until one is decided. */
export function restoreAnswer(
  accountIdB64: string,
  storage: RestoredMarkerStorage | null = defaultStorage(),
): RestoreAnswer | null {
  if (storage === null || accountIdB64.length === 0) return null;
  try {
    const raw = storage.getItem(ANSWER_PREFIX + accountIdB64);
    return raw === "answered" || raw === "founded" ? raw : null;
  } catch {
    return null;
  }
}

/** What the chat list shows for a restored device: the card, or the
 *  ordinary list. */
export type RestoredListState = "none" | "card";

/**
 * `docs/ux/identity-recovery.md` section 7.2 (amended 2026-11-11). The card
 * only when this device founded, the backup named a conversation and none is
 * on the list yet; otherwise the ordinary list. Another device answering adds
 * nothing to the list (a person cannot act on that background work), and an
 * answer not known yet claims nothing.
 */
export function restoredListState(facts: {
  readonly marked: boolean;
  readonly knownConversations: number;
  readonly conversationCount: number;
  readonly answer: RestoreAnswer | null;
}): RestoredListState {
  if (!facts.marked || facts.conversationCount > 0 || facts.knownConversations <= 0) return "none";
  return facts.answer === "founded" ? "card" : "none";
}
