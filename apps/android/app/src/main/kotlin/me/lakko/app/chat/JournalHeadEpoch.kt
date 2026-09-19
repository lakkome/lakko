// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/**
 * The epoch a non-member must target to join a group, read from the journal
 * HEAD's public framing (ADR-0028 section 3) after paging to the end.
 *
 * Two things went wrong live on 2026-09-19 and this closes both. The joiner
 * fetched the journal once with `after = 0` and took the last row of that
 * page, but the DS pages at 100 rows by default and the group had 370, so the
 * "head" was a row from the morning and the epoch was many commits stale:
 * every member refused the proposal as the wrong epoch and nobody told the
 * joiner (the page even said `hasMore`). And a Commit row is framed at the
 * epoch it was created in while the group it leaves behind is one higher, so
 * a journal whose head IS a Commit needs `epochAfterMessage`, not
 * `epochOfMessage`.
 */
public object JournalHeadEpoch {
    public class Row(public val id: Long, public val payload: ByteArray)

    public class Page(public val rows: List<Row>, public val hasMore: Boolean)

    /** Rows per page: the DS maximum, so a long journal costs few round trips. */
    public const val PAGE_SIZE: Int = 1000

    /** A bound on the walk, so a server that always says `hasMore` cannot hang
     *  the joiner. */
    public const val MAX_PAGES: Int = 1000

    /**
     * Walk the whole journal from its first row, page by page, handing every
     * row to [visit]; the first non-null value [visit] returns ends the walk
     * and is the result, `null` when the journal ends first. This is the one
     * paging loop every non-member read of a journal goes through: reading
     * the head epoch, and waiting for the Welcome an admin posts after
     * approving a join, which had the same single-page defect and so never
     * saw a Welcome on a journal longer than one page. A fetch error
     * propagates.
     */
    public suspend fun <T : Any> scan(
        fetchPage: suspend (after: Long, limit: Int) -> Page,
        visit: (Row) -> T?,
    ): T? {
        var after = 0L
        repeat(MAX_PAGES) {
            val page = fetchPage(after, PAGE_SIZE)
            for (row in page.rows) {
                visit(row)?.let { return it }
            }
            // Stop on the last page, and on a page that did not move the
            // cursor (a server bug must not become an endless loop here).
            val last = page.rows.lastOrNull() ?: return null
            if (!page.hasMore || last.id <= after) return null
            after = last.id
        }
        return null
    }

    /**
     * Walk the journal to its head and return the epoch the group is in after
     * the last MLS row, or `0` for an empty journal. [epochAfter] is
     * `epochAfterMessage`: `null` for a non-MLS row, which is skipped. A fetch
     * error propagates; the caller decides what a failed read means.
     */
    public suspend fun read(
        fetchPage: suspend (after: Long, limit: Int) -> Page,
        epochAfter: (ByteArray) -> ULong?,
    ): ULong {
        var epoch = 0uL
        scan<Unit>(fetchPage) { row ->
            epochAfter(row.payload)?.let { epoch = it }
            null
        }
        return epoch
    }
}
