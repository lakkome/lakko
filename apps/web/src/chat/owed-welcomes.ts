// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0050 section 2.5a item 7 - which groups hold a Welcome fan-out the sweep
 * merged but could not deliver (its ADR-0040 outbox entry carries
 * `sweepWelcomesOwed`). A Welcome is kept owed when no reach grant this
 * account holds opens the addressee's closed mailbox; it is retried at the
 * tail of every poll of its group, and, through this, as soon as a new grant
 * arrives, for every group that owes one.
 */

/** The outbox fields the check reads. */
export interface OwedWelcomeEntry {
  readonly sweepWelcomesOwed?: boolean;
}

/** The ids, in order, of the groups whose un-acked entries owe a Welcome. A
 *  group whose outbox could not be read is skipped (its own poll retries). */
export async function groupsOwingWelcomes(
  groupIdsB64: readonly string[],
  loadUnacked: (groupIdB64: string) => Promise<readonly OwedWelcomeEntry[]>,
): Promise<string[]> {
  const out: string[] = [];
  for (const groupIdB64 of groupIdsB64) {
    try {
      const entries = await loadUnacked(groupIdB64);
      if (entries.some((e) => e.sweepWelcomesOwed === true)) out.push(groupIdB64);
    } catch {
      /* unreadable: the group's own poll retries */
    }
  }
  return out;
}
