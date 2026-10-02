// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Three words from your phrase": the pure rules of the local three-word
 * check (maintainer decision 2026-10-02, which supersedes ADR-0064
 * decision 5 and the three-word sections of docs/ux/identity-recovery.md).
 *
 * The check is LOCAL, like a wallet's "enter words 7, 3 and 12". A device
 * that holds the account's keys also holds the recovery phrase under its
 * device key. Forgot the password on such a device: the client draws three
 * fresh random positions, the person types those words, the client compares
 * them with the phrase it holds, and on a match signs in with the seed it
 * already has. The server takes no part in the check and stores nothing
 * about the words.
 *
 * Two rules live here, both pure so vitest (plain node, no Solid transform)
 * can pin them:
 *
 *   - `offersThreeWords`: whether the row is on the recovery list at all.
 *   - the guess budget: five misses, then a 15-minute pause.
 *
 * ## What the budget is, stated plainly
 *
 * The budget is a CLIENT-SIDE gate in front of a credential this device
 * already holds. The phrase sits on the device, wrapped under a
 * non-extractable device key that the same browser profile can use without
 * the password. Someone who can run their own code in this profile does not
 * need to guess three words: they can ask the device key to open the phrase.
 * The budget therefore protects against exactly one thing: a person at the
 * unlocked device typing guesses into this form. It is not a server limit
 * and must never be described as one.
 */
import type { MarkerStorage } from "../phrase-gate";

/** How many words the person answers with. */
export const THREE_WORDS = 3;

// ---------------------------------------------------------------------------
// Row visibility
// ---------------------------------------------------------------------------

/**
 * Whether the recovery list offers "Three words from your phrase" on this
 * device. Every input is a local fact, so the answer says nothing about any
 * account on the server (no oracle):
 *
 *   - an account is cached on this device and has a username (the sign-in
 *     that follows the check is addressed by the name's login handle; a
 *     guest account has none);
 *   - the device holds the recovery phrase for that account (its device key
 *     opens it);
 *   - the device holds the account's keys (else the sign-in would only
 *     reach "Restore your keys", which three words cannot answer).
 *
 * On a device where this is `false` the row is absent and the note under
 * the list does not mention three words.
 */
export function offersThreeWords(facts: {
  /** The cached account's username; `null` when no account is cached. */
  readonly cachedUsername: string | null;
  readonly phraseOnDevice: boolean;
  readonly keysOnDevice: boolean;
}): boolean {
  return (
    facts.cachedUsername !== null &&
    facts.cachedUsername.trim().length > 0 &&
    facts.phraseOnDevice &&
    facts.keysOnDevice
  );
}

// ---------------------------------------------------------------------------
// Guess budget
// ---------------------------------------------------------------------------

/** Misses allowed before the route pauses. */
export const THREE_WORDS_MAX_MISSES = 5;
/** How long the route pauses after the last allowed miss. */
export const THREE_WORDS_PAUSE_MS = 15 * 60 * 1000;

/** The budget for one account on this device. */
export interface ThreeWordsBudget {
  /** Misses since the last success or the last pause. */
  readonly misses: number;
  /** When the pause ends (unix ms), or `null` when the route is open. */
  readonly pausedUntilMs: number | null;
}

export const FRESH_BUDGET: ThreeWordsBudget = { misses: 0, pausedUntilMs: null };

/** The budget as it stands at `nowMs`: a pause that has run out is over and
 *  the count starts again. */
export function budgetAt(budget: ThreeWordsBudget, nowMs: number): ThreeWordsBudget {
  if (budget.pausedUntilMs !== null && nowMs >= budget.pausedUntilMs) return FRESH_BUDGET;
  return budget;
}

/** Milliseconds of pause left at `nowMs`; 0 when the route is open. A
 *  device clock set back cannot stretch a pause past its full length. */
export function pauseRemainingMs(budget: ThreeWordsBudget, nowMs: number): number {
  const current = budgetAt(budget, nowMs);
  if (current.pausedUntilMs === null) return 0;
  return Math.min(THREE_WORDS_PAUSE_MS, Math.max(0, current.pausedUntilMs - nowMs));
}

/** Tries left before the pause, at `nowMs` (0 while paused). */
export function triesLeft(budget: ThreeWordsBudget, nowMs: number): number {
  const current = budgetAt(budget, nowMs);
  if (current.pausedUntilMs !== null) return 0;
  return Math.max(0, THREE_WORDS_MAX_MISSES - current.misses);
}

/** The budget after one more miss at `nowMs`. The fifth miss starts the
 *  pause. A miss while paused does not happen (the form is closed) and
 *  changes nothing. */
export function afterMiss(budget: ThreeWordsBudget, nowMs: number): ThreeWordsBudget {
  const current = budgetAt(budget, nowMs);
  if (current.pausedUntilMs !== null) return current;
  const misses = current.misses + 1;
  return misses >= THREE_WORDS_MAX_MISSES
    ? { misses, pausedUntilMs: nowMs + THREE_WORDS_PAUSE_MS }
    : { misses, pausedUntilMs: null };
}

// ---------------------------------------------------------------------------
// Persistence: localStorage, per account, so a reload does not reset it
// ---------------------------------------------------------------------------

function budgetKey(accountIdB64: string): string {
  return `lakko:three-words-budget:${accountIdB64}`;
}

/** The stored budget, or a fresh one when nothing (or nothing readable) is
 *  stored. A malformed row reads as fresh rather than locking the route
 *  forever; a row whose pause lies further ahead than a full pause (a clock
 *  that was set back) is clamped by {@link pauseRemainingMs}. */
export function loadBudget(storage: MarkerStorage | undefined, accountIdB64: string): ThreeWordsBudget {
  try {
    const raw = storage?.getItem(budgetKey(accountIdB64)) ?? null;
    if (raw === null) return FRESH_BUDGET;
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return FRESH_BUDGET;
    const { misses, pausedUntilMs } = parsed as { misses?: unknown; pausedUntilMs?: unknown };
    if (typeof misses !== "number" || !Number.isInteger(misses) || misses < 0) return FRESH_BUDGET;
    if (pausedUntilMs !== null && (typeof pausedUntilMs !== "number" || !Number.isFinite(pausedUntilMs))) {
      return FRESH_BUDGET;
    }
    return { misses: Math.min(misses, THREE_WORDS_MAX_MISSES), pausedUntilMs: pausedUntilMs ?? null };
  } catch {
    return FRESH_BUDGET;
  }
}

export function saveBudget(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  budget: ThreeWordsBudget,
): void {
  try {
    storage?.setItem(budgetKey(accountIdB64), JSON.stringify(budget));
  } catch {
    // Storage blocked: the form's in-memory copy still holds for this page.
  }
}

/** A success, a forget-device and an account deletion clear the budget. */
export function clearBudget(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(budgetKey(accountIdB64));
  } catch {
    // Storage blocked: nothing was recorded either.
  }
}

// ---------------------------------------------------------------------------
// Presentation helpers
// ---------------------------------------------------------------------------

/** A wait as `m:ss` (Latin digits in every language, like the word
 *  numbers), rounded up to the next second so it never reads 0:00 while
 *  the wait still runs. */
export function formatCountdown(remainingMs: number): string {
  const totalSecs = Math.max(0, Math.ceil(remainingMs / 1000));
  const minutes = Math.floor(totalSecs / 60);
  const seconds = totalSecs % 60;
  return `${minutes}:${String(seconds).padStart(2, "0")}`;
}
