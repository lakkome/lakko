// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A paged walk over a DS journal (a group journal, a knock queue, a mailbox).
 *
 * This module used to carry `journalHeadEpoch`, the reading of a group's epoch
 * off the journal head's public framing that the `/g/` joiner needed (ADR-0028
 * section 3; the 2026-09-19 single-page defect it closed is why the walk below
 * pages to the end). ADR-0053 removed that read: a claimed journal answers only
 * its members, and the joiner learns the epoch from the knock's response
 * instead. What is left is the one paging loop every multi-page read shares.
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
 *  the caller. */
export const JOURNAL_HEAD_MAX_PAGES = 1000;

/**
 * Walk the whole journal from its first row, page by page, handing every row
 * to `visit`; the first defined value `visit` returns ends the walk and is
 * the result. `undefined` when the journal ends first. Used by the join
 * attribution read over a group's knock queue and by the co-publisher's wait
 * for its Welcome in its own mailbox. Errors from `fetchPage` propagate.
 */
export async function scanJournal<T>(
  fetchPage: (after: number, limit: number) => Promise<JournalHeadPage>,
  visit: (row: JournalHeadRow) => T | undefined,
  /** Read only rows after this id (a caller that keeps its own cursor). */
  startAfter = 0,
): Promise<T | undefined> {
  let after = startAfter;
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
