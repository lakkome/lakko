// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The epoch a non-member must target to join a group, read from the journal
 * HEAD's public framing (ADR-0028 section 3) after paging to the end.
 *
 * Two things went wrong live on 2026-09-19 and this closes both. The joiner
 * fetched the journal once with `after=0` and took the last row of that
 * page, but the DS pages at 100 rows by default and the group had 370, so the
 * "head" was a row from the morning and the epoch was many commits stale:
 * every member refused the proposal as the wrong epoch and nobody told the
 * joiner (the page even said `hasMore`). And a Commit row is framed at the
 * epoch it was created in while the group it leaves behind is one higher, so
 * a journal whose head IS a Commit needs `epochAfterMessage`, not
 * `epochOfMessage`.
 */
export interface JournalHeadRow {
  readonly id: number;
  readonly payload: Uint8Array;
}

export interface JournalHeadPage {
  readonly messages: readonly JournalHeadRow[];
  readonly hasMore: boolean;
}

/** Rows per page: the DS maximum, so a long journal costs few round trips. */
export const JOURNAL_HEAD_PAGE_SIZE = 1000;
/** A bound on the walk, so a server that always says `hasMore` cannot hang
 *  the joiner. */
export const JOURNAL_HEAD_MAX_PAGES = 1000;

/**
 * Walk the whole journal from its first row, page by page, handing every row
 * to `visit`; the first defined value `visit` returns ends the walk and is
 * the result. `undefined` when the journal ends first. This is the one
 * paging loop every non-member read of a journal goes through: reading the
 * head epoch, and waiting for the Welcome an admin posts after approving a
 * join, which had the same single-page defect and so never saw a Welcome on
 * a journal longer than one page. Errors from `fetchPage` propagate.
 */
export async function scanJournal<T>(
  fetchPage: (after: number, limit: number) => Promise<JournalHeadPage>,
  visit: (row: JournalHeadRow) => T | undefined,
): Promise<T | undefined> {
  let after = 0;
  for (let page = 0; page < JOURNAL_HEAD_MAX_PAGES; page++) {
    const raw = await fetchPage(after, JOURNAL_HEAD_PAGE_SIZE);
    for (const m of raw.messages) {
      const hit = visit(m);
      if (hit !== undefined) return hit;
    }
    const last = raw.messages[raw.messages.length - 1];
    // Stop on the last page, and on a page that did not move the cursor (a
    // server bug must not become an endless loop here).
    if (last === undefined || !raw.hasMore || last.id <= after) return undefined;
    after = last.id;
  }
  return undefined;
}

/**
 * Walk the journal to its head and return the epoch the group is in after the
 * last MLS row, or `0n` for an empty journal (a fresh group). `epochAfter` is
 * `epochAfterMessage`: `undefined` for a non-MLS row, which is skipped.
 * Errors from `fetchPage` propagate; the caller decides what a failed read
 * means.
 */
export async function journalHeadEpoch(
  fetchPage: (after: number, limit: number) => Promise<JournalHeadPage>,
  epochAfter: (payload: Uint8Array) => bigint | undefined,
): Promise<bigint> {
  let epoch = 0n;
  await scanJournal<never>(fetchPage, (m) => {
    const e = epochAfter(m.payload);
    if (e !== undefined) epoch = e;
    return undefined;
  });
  return epoch;
}
