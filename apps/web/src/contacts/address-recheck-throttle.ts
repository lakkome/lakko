// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The throttle of the address re-check (ADR-0074 section 3.5a rule 6).
 *
 * A refused write to a person's mailbox re-checks that person's identity
 * bundle once. Several writes to one person can be refused in a row (a
 * Welcome per group, a profile per conversation), and each would fetch the
 * same bundle; so a person is re-checked at most once in five minutes. The
 * same number on every client (chosen, not measured).
 *
 * Held in memory, per page: a reload may re-check again, which is one fetch.
 */

/** The shortest time between two re-checks of one person. */
export const ADDRESS_RECHECK_MIN_INTERVAL_MS = 5 * 60 * 1000;

/**
 * Whether a person last re-checked at `lastAtMs` (`null`: never) may be
 * re-checked at `nowMs`. A last time in the future (the clock went
 * backwards) reads as "just re-checked". Pure.
 */
export function addressRecheckAllowed(lastAtMs: number | null, nowMs: number): boolean {
  if (lastAtMs === null) return true;
  if (!Number.isFinite(lastAtMs) || lastAtMs > nowMs) return false;
  return nowMs - lastAtMs >= ADDRESS_RECHECK_MIN_INTERVAL_MS;
}

/** person (account id, base64) -> when their address was last re-checked.
 *  Module-level: every `ContactsClient` on the page shares it. */
export const lastAddressRecheckAtMs = new Map<string, number>();
