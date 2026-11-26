// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 walk 2026-11-26 (D4) - the successors of a group whose name this
 * device carried in (ADR-0073 R.7 case 3, `GroupNameContext.carryOver`): the
 * carry is sent again on every continuity pass until one send went out, and
 * never after. Per account, in localStorage; a lost record costs one more
 * send, which the receiver drops (the stamp is not newer).
 */

function carriedKey(accountIdB64: string): string {
  return `lakko:continuity-name-carried:${accountIdB64}`;
}

/** How many successors are remembered (the oldest go first; chosen). */
const MAX_CARRIED = 512;

/** Whether the name was already sent into `successorGroupIdB64`. */
export function nameCarriedInto(accountIdB64: string, successorGroupIdB64: string): boolean {
  try {
    const raw = globalThis.localStorage?.getItem(carriedKey(accountIdB64)) ?? null;
    return raw !== null && (JSON.parse(raw) as unknown[]).includes(successorGroupIdB64);
  } catch {
    return false;
  }
}

/** Record that it was. */
export function markNameCarried(accountIdB64: string, successorGroupIdB64: string): void {
  try {
    const raw = globalThis.localStorage?.getItem(carriedKey(accountIdB64)) ?? null;
    const held = raw === null ? [] : (JSON.parse(raw) as unknown[]).filter((x): x is string => typeof x === "string");
    if (!held.includes(successorGroupIdB64)) held.push(successorGroupIdB64);
    globalThis.localStorage?.setItem(carriedKey(accountIdB64), JSON.stringify(held.slice(-MAX_CARRIED)));
  } catch {
    /* sent again at the next pass: the receiver keeps only a newer stamp */
  }
}

/** The storage key, for the account wipe. */
export function nameCarriedStorageKey(accountIdB64: string): string {
  return carriedKey(accountIdB64);
}
