// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

/*
 * Reading a whole rendezvous id (security follow-up of 2026-11-21).
 *
 * A rendezvous id is readable and writable by anyone who learned it (a write
 * spends one `rendezvous-write` token), so a reader that looks at one page
 * can have the row it waits for buried under rows that are not it. A reader
 * that needs every row (the re-entry gap-history look) reads page after
 * page: `after` = the last row id of the previous page, [RENDEZVOUS_PAGE_LIMIT]
 * rows a page, following `has_more`, at most [RENDEZVOUS_PAGE_CAP] pages.
 * The same numbers as web and iOS.
 */

/** Rows asked for per page of a rendezvous read. */
public const val RENDEZVOUS_PAGE_LIMIT: Int = 100

/** Pages read at most (1000 rows). Chosen, not measured. */
public const val RENDEZVOUS_PAGE_CAP: Int = 10

/**
 * Every row of a rendezvous, oldest first, page by page through [fetchPage]
 * (`after`, `limit`), stopping when a page says there is no more, comes back
 * empty, or after [RENDEZVOUS_PAGE_CAP] pages. A failed page throws.
 */
public suspend fun readRendezvousPages(
    fetchPage: suspend (after: Long, limit: Int) -> FetchResult,
): List<MailboxMessage> {
    val rows = ArrayList<MailboxMessage>()
    var after = 0L
    for (page in 0 until RENDEZVOUS_PAGE_CAP) {
        val result = fetchPage(after, RENDEZVOUS_PAGE_LIMIT)
        rows += result.messages
        if (!result.hasMore || result.messages.isEmpty()) break
        val last = result.messages.maxOf { it.id }
        // A cursor that does not move would read the same page again.
        if (last <= after) break
        after = last
    }
    return rows
}

/** [readRendezvousPages] over [DsClient.fetchRendezvous], no long poll. */
public suspend fun DsClient.readWholeRendezvous(rendezvous: ByteArray): List<MailboxMessage> =
    readRendezvousPages { after, limit ->
        fetchRendezvous(rendezvous, after = after, limit = limit, waitSecs = 0)
    }
