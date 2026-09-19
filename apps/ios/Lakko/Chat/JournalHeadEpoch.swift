// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The epoch a non-member must target to join a group, read from the journal
/// HEAD's public framing (ADR-0028 section 3) after paging to the end.
///
/// Two things went wrong live on 2026-09-19 and this closes both. The joiner
/// fetched the journal once with `after: 0` and took the last row of that
/// page, but the DS pages at 100 rows by default and the group had 370, so the
/// "head" was a row from the morning and the epoch was many commits stale:
/// every member refused the proposal as the wrong epoch and nobody told the
/// joiner (the page even said `hasMore`). And a Commit row is framed at the
/// epoch it was created in while the group it leaves behind is one higher, so
/// a journal whose head IS a Commit needs `epochAfterMessage`, not
/// `epochOfMessage`.
enum JournalHeadEpoch {
    struct Row {
        let id: Int64
        let payload: Data
    }

    struct Page {
        let rows: [Row]
        let hasMore: Bool
    }

    /// Rows per page: the DS maximum, so a long journal costs few round trips.
    static let pageSize = 1000
    /// A bound on the walk, so a server that always says `hasMore` cannot hang
    /// the joiner.
    static let maxPages = 1000

    /// Walk the whole journal from its first row, page by page, handing every
    /// row to `visit`; the first non-nil value `visit` returns ends the walk
    /// and is the result, `nil` when the journal ends first. This is the one
    /// paging loop every non-member read of a journal goes through: reading
    /// the head epoch, and waiting for the Welcome an admin posts after
    /// approving a join, which had the same single-page defect and so never
    /// saw a Welcome on a journal longer than one page. A fetch error
    /// propagates.
    static func scan<T>(
        fetchPage: (Int64, Int) async throws -> Page,
        visit: (Row) throws -> T?
    ) async throws -> T? {
        var after: Int64 = 0
        for _ in 0..<maxPages {
            let page = try await fetchPage(after, pageSize)
            for row in page.rows {
                if let hit = try visit(row) { return hit }
            }
            // Stop on the last page, and on a page that did not move the
            // cursor (a server bug must not become an endless loop here).
            guard let last = page.rows.last, page.hasMore, last.id > after else { return nil }
            after = last.id
        }
        return nil
    }

    /// Walk the journal to its head and return the epoch the group is in after
    /// the last MLS row, or `0` for an empty journal. `epochAfter` is
    /// `epochAfterMessage`: `nil` for a non-MLS row, which is skipped. A fetch
    /// error propagates; the caller decides what a failed read means.
    static func read(
        fetchPage: (Int64, Int) async throws -> Page,
        epochAfter: (Data) -> UInt64?
    ) async throws -> UInt64 {
        var epoch: UInt64 = 0
        let _: Never? = try await scan(fetchPage: fetchPage) { row in
            if let e = epochAfter(row.payload) { epoch = e }
            return nil
        }
        return epoch
    }
}
