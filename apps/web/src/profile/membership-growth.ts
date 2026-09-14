// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §2 trigger 3 — did a group GAIN a member?
 *
 * Decided as a set difference over member signature keys, the same way the
 * ADR-0043 shrink detection decides a removal: not a count (a remove and an
 * add in one commit leave the count unchanged) and not leaf indices (OpenMLS
 * hands a vacated leaf to the next member added). A key present now that was
 * absent before is a newcomer, whatever else changed.
 *
 * `previous === null` means "no baseline yet" — the first observation of a
 * group is not growth (this device joining is trigger 1, not trigger 3). A
 * torn roster read (an empty `current`) is never growth either.
 */
export function membersGrew(
  previous: ReadonlySet<string> | null,
  current: ReadonlySet<string>,
): boolean {
  if (previous === null || current.size === 0) return false;
  for (const key of current) {
    if (!previous.has(key)) return true;
  }
  return false;
}
