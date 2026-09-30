// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 6 - the committer candidates a proposer delegates to.
 *
 * Under ADR-0045 the member who proposes an Add may not be the one who
 * commits it, and the committer may hold no key for the added person. So the
 * proposer, who does, signs one single-use delegation per committer
 * candidate - the owner, then each admin by committer rank - each naming that
 * candidate's account key as the delegate. At most `max` (16 in the core).
 */

/** Delegation lifetime the proposer asks for: just under the DS's 24-hour
 *  maximum, so clock skew between the proposer and the DS never pushes a
 *  fresh delegation past the bound. Chosen, not measured. */
export const DELEGATION_TTL_MS = 23 * 60 * 60 * 1000;

/**
 * The candidates, owner first, then admins in ascending committer rank
 * (unranked admins last, in table order), duplicates removed, capped at
 * `max`. Keys compared as hex.
 */
export function committerCandidates(
  ownerHex: string | null,
  adminHexes: readonly string[],
  rankOf: (hex: string) => number | undefined,
  max: number,
): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  const push = (h: string): void => {
    if (seen.has(h) || out.length >= max) return;
    seen.add(h);
    out.push(h);
  };
  if (ownerHex !== null) push(ownerHex);
  const ranked = adminHexes
    .map((h, i) => ({ h, i, r: rankOf(h) }))
    .sort((a, b) => {
      const ra = a.r ?? Number.MAX_SAFE_INTEGER;
      const rb = b.r ?? Number.MAX_SAFE_INTEGER;
      return ra !== rb ? ra - rb : a.i - b.i;
    });
  for (const { h } of ranked) push(h);
  return out;
}
