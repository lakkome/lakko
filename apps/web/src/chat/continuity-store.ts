// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.5 / R.7 - the two facts about a moved conversation that are
 * this device's alone and that nothing else records:
 *
 *   * WHEN this device learned an old group was continued elsewhere: the
 *     start of the window in which an old GROUP's journal is still read
 *     (`lineage.ts` `oldSegmentPolled`);
 *   * that the founder's re-send of its own messages into the successor of
 *     a one-to-one RAN (`reset-repair.ts` `messagesToResend`), so a second
 *     pass never sends them twice, and how far an interrupted one got (the
 *     original id of the last message that landed), so it continues there;
 *   * the first half of a member's device swap that landed in a commit of
 *     its own, so the second half leaves no `Members changed` row either.
 *
 * `localStorage`, per account, group ids and timestamps only. Removed with
 * the account's data (`auth/wipe-account.ts`).
 */

/** The row's key; the account id (standard base64) follows the prefix. */
export const CONTINUITY_STATE_PREFIX = "lakko.reset-continuity.v1.";

interface Row {
  /** base64 old group id -> when this device learned it moved (ms). */
  moved: Record<string, number>;
  /** base64 old group id -> the re-send into its successor ran. */
  resent: Record<string, true>;
  /** base64 group id -> the people whose superseded devices left it and
   *  whose new devices have not arrived (`reset-repair.ts`
   *  `deviceSwapAcrossCommits`): account -> since when (ms). */
  swapping: Record<string, Record<string, number>>;
  /** base64 old group id -> its push subscription was retired here
   *  (`lineage.ts` `retiresOldSegmentPush`). */
  pushRetired: Record<string, true>;
  /** base64 old group id -> the ORIGINAL message id of the last re-sent
   *  message that landed in the successor: where an interrupted re-send
   *  continues (`reset-repair.ts` `resendRemaining`). */
  resendCursor: Record<string, number>;
}

function load(accountIdB64: string): Row {
  try {
    const raw = localStorage.getItem(CONTINUITY_STATE_PREFIX + accountIdB64);
    if (raw === null) return { moved: {}, resent: {}, swapping: {}, pushRetired: {}, resendCursor: {} };
    const parsed = JSON.parse(raw) as Partial<Row>;
    return {
      moved: typeof parsed.moved === "object" && parsed.moved !== null ? parsed.moved : {},
      resent: typeof parsed.resent === "object" && parsed.resent !== null ? parsed.resent : {},
      swapping:
        typeof parsed.swapping === "object" && parsed.swapping !== null ? parsed.swapping : {},
      pushRetired:
        typeof parsed.pushRetired === "object" && parsed.pushRetired !== null ? parsed.pushRetired : {},
      resendCursor:
        typeof parsed.resendCursor === "object" && parsed.resendCursor !== null
          ? parsed.resendCursor
          : {},
    };
  } catch {
    return { moved: {}, resent: {}, swapping: {}, pushRetired: {}, resendCursor: {} };
  }
}

function save(accountIdB64: string, row: Row): void {
  try {
    localStorage.setItem(CONTINUITY_STATE_PREFIX + accountIdB64, JSON.stringify(row));
  } catch {
    /* storage unavailable: the facts are per session then */
  }
}

/** Record that `oldGroupIdB64` moved; the FIRST time wins. */
export function noteConversationMoved(accountIdB64: string, oldGroupIdB64: string, nowMs: number): void {
  const row = load(accountIdB64);
  if (typeof row.moved[oldGroupIdB64] === "number") return;
  row.moved[oldGroupIdB64] = nowMs;
  save(accountIdB64, row);
}

/** When this device learned `oldGroupIdB64` moved, or `null`. */
export function conversationMovedAt(accountIdB64: string, oldGroupIdB64: string): number | null {
  const at = load(accountIdB64).moved[oldGroupIdB64];
  return typeof at === "number" && Number.isFinite(at) ? at : null;
}

/** The re-send into the successor of `oldGroupIdB64` ran. */
export function markResent(accountIdB64: string, oldGroupIdB64: string): void {
  const row = load(accountIdB64);
  row.resent[oldGroupIdB64] = true;
  save(accountIdB64, row);
}

export function wasResent(accountIdB64: string, oldGroupIdB64: string): boolean {
  return load(accountIdB64).resent[oldGroupIdB64] === true;
}

/**
 * The re-send of `oldGroupIdB64` got as far as the original `messageId`:
 * that message LANDED in the successor. Monotone: a lower id never moves
 * the cursor back. Written after every message, so an interruption costs
 * at most the message in flight.
 */
export function advanceResendCursor(accountIdB64: string, oldGroupIdB64: string, messageId: number): void {
  if (!Number.isSafeInteger(messageId) || messageId < 0) return;
  const row = load(accountIdB64);
  const held = row.resendCursor[oldGroupIdB64];
  if (typeof held === "number" && Number.isFinite(held) && held >= messageId) return;
  row.resendCursor[oldGroupIdB64] = messageId;
  save(accountIdB64, row);
}

/** Where the re-send of `oldGroupIdB64` continues after, or `null` when no
 *  message of it has landed. */
export function resendCursorOf(accountIdB64: string, oldGroupIdB64: string): number | null {
  const at = load(accountIdB64).resendCursor[oldGroupIdB64];
  return typeof at === "number" && Number.isSafeInteger(at) && at >= 0 ? at : null;
}

/** The old group's push subscription was retired on this device. */
export function markPushRetired(accountIdB64: string, oldGroupIdB64: string): void {
  const row = load(accountIdB64);
  if (row.pushRetired[oldGroupIdB64] === true) return;
  row.pushRetired[oldGroupIdB64] = true;
  save(accountIdB64, row);
}

export function wasPushRetired(accountIdB64: string, oldGroupIdB64: string): boolean {
  return load(accountIdB64).pushRetired[oldGroupIdB64] === true;
}

/** The first halves of a device swap this device saw in `groupIdB64`. */
export function awaitedSwaps(accountIdB64: string, groupIdB64: string): Record<string, number> {
  const held = load(accountIdB64).swapping[groupIdB64];
  if (typeof held !== "object" || held === null) return {};
  const out: Record<string, number> = {};
  for (const [person, sinceMs] of Object.entries(held)) {
    if (typeof sinceMs === "number" && Number.isFinite(sinceMs)) out[person] = sinceMs;
  }
  return out;
}

/** Replace that memory (an empty one removes the group's entry). */
export function setAwaitedSwaps(
  accountIdB64: string,
  groupIdB64: string,
  awaited: Readonly<Record<string, number>>,
): void {
  const row = load(accountIdB64);
  const had = row.swapping[groupIdB64];
  if (Object.keys(awaited).length === 0) {
    if (had === undefined) return;
    delete row.swapping[groupIdB64];
  } else {
    if (JSON.stringify(had) === JSON.stringify(awaited)) return;
    row.swapping[groupIdB64] = { ...awaited };
  }
  save(accountIdB64, row);
}

/** Forget everything for the account (its data is wiped). */
export function clearContinuityStateForAccount(accountIdB64: string): void {
  try {
    localStorage.removeItem(CONTINUITY_STATE_PREFIX + accountIdB64);
  } catch {
    /* nothing to clear */
  }
}
