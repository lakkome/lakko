// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * When this device first held each conversation group, per account
 * (base scenario 9: one person is one row).
 *
 * The duplicate-one-to-one rule (`duplicate-one-to-one.ts`) needs to know
 * which of two one-to-ones with the same person is the newer one when both
 * were founded by the same side (the person founded a new one after losing
 * theirs) or by another device of this account. Neither the group nor its
 * founder says when it was made, so the device records the first time it
 * holds each group: a group joined later is recorded later. A group seen for
 * the first time together with others (the first run after an upgrade, a
 * restore) shares their time and is ordered by nothing, which leaves the
 * rule as it was.
 *
 * Device-local, in `localStorage`; never sent. A failed read or write reads
 * as "unknown" (the rule then orders nothing).
 */

const PREFIX = "lakko:conversation-held-since:";

/** The stored map, or an empty one. */
function load(accountIdB64: string): Record<string, number> {
  try {
    if (typeof localStorage === "undefined") return {};
    const raw = localStorage.getItem(PREFIX + accountIdB64);
    if (raw === null) return {};
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return {};
    const out: Record<string, number> = {};
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof v === "number" && Number.isFinite(v)) out[k] = v;
    }
    return out;
  } catch {
    return {};
  }
}

/**
 * Record `nowMs` for every group in `groupHexes` this account has no time for
 * yet, and return the time of each of them (`undefined` when it could not be
 * kept). Groups no longer held are forgotten.
 */
export function heldSince(
  accountIdB64: string,
  groupHexes: readonly string[],
  nowMs: number,
): Map<string, number | undefined> {
  const stored = load(accountIdB64);
  const next: Record<string, number> = {};
  let changed = false;
  for (const hex of groupHexes) {
    const known = stored[hex];
    if (known === undefined) changed = true;
    next[hex] = known ?? nowMs;
  }
  if (Object.keys(stored).length !== groupHexes.length) changed = true;
  let kept = true;
  if (changed) {
    try {
      if (typeof localStorage === "undefined") kept = false;
      else localStorage.setItem(PREFIX + accountIdB64, JSON.stringify(next));
    } catch {
      kept = false;
    }
  }
  const out = new Map<string, number | undefined>();
  for (const hex of groupHexes) out.set(hex, kept || stored[hex] !== undefined ? next[hex] : undefined);
  return out;
}

/** Forget device / delete account: drop the account's record. */
export function clearHeldSince(accountIdB64: string): void {
  try {
    if (typeof localStorage !== "undefined") localStorage.removeItem(PREFIX + accountIdB64);
  } catch {
    /* nothing kept */
  }
}
