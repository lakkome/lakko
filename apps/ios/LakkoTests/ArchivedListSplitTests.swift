// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// `docs/ux/navigation.md` section 2.2 step 3 ("Archived, as built
/// (2026-11-25)"), asserted THROUGH the shared rule (`archived_section` in
/// `crates/lakko-ui-rules`, reached over UniFFI): archived conversations are
/// never in the chats list; one bar at the bottom opens them; search lists
/// archived matches under a divider.
final class ArchivedListSplitTests: XCTestCase {

    private struct Row: Equatable {
        let id: String
        let archived: Bool
        var pinned = false
    }

    private func live(_ id: String) -> Row { Row(id: id, archived: false) }
    private func gone(_ id: String) -> Row { Row(id: id, archived: true) }
    private func archivedRows(_ n: Int) -> [Row] { (0..<n).map { gone("a\($0)") } }

    private func split(
        _ rows: [Row], searching: Bool = false, archiveOpen: Bool = false
    ) -> ArchivedListSplit.Split<Row> {
        ArchivedListSplit.split(
            rows, isArchived: { $0.archived }, isPinned: { $0.pinned },
            searching: searching, archiveOpen: archiveOpen)
    }

    func testNoBarWhenNothingIsArchived() {
        let s = split([live("x"), live("y")])
        XCTAssertEqual(s.pane, .chats)
        XCTAssertNil(s.barCount)
        XCTAssertFalse(s.divider)
        XCTAssertEqual(s.top.map(\.id), ["x", "y"])
        XCTAssertTrue(s.archived.isEmpty)
        XCTAssertNil(split([]).barCount)
    }

    func testOneArchivedConversationIsBehindTheBar() {
        let s = split([live("x"), gone("a")])
        XCTAssertEqual(s.pane, .chats)
        XCTAssertEqual(s.top.map(\.id), ["x"])
        XCTAssertEqual(s.barCount, 1)
        XCTAssertFalse(s.divider)
        XCTAssertEqual(s.archived.map(\.id), ["a"])
    }

    func testManyArchivedCarryTheirCount() {
        let s = split([live("x")] + archivedRows(20))
        XCTAssertEqual(s.barCount, 20)
        XCTAssertEqual(s.top.map(\.id), ["x"])
    }

    func testPinnedArchivedConversationStaysWithThePins() {
        var pinned = gone("p")
        pinned.pinned = true
        let s = split([pinned, live("x"), gone("a")])
        XCTAssertEqual(s.top.map(\.id), ["p", "x"])
        XCTAssertEqual(s.archived.map(\.id), ["a"])
        XCTAssertEqual(s.barCount, 1)
    }

    func testOpeningTheBarShowsTheArchiveInOrder() {
        let rows = archivedRows(3)
        let s = split([live("x")] + rows, archiveOpen: true)
        XCTAssertEqual(s.pane, .archive)
        XCTAssertNil(s.barCount)
        XCTAssertFalse(s.divider)
        XCTAssertEqual(s.archived.map(\.id), rows.map(\.id))
    }

    func testUnarchivingTheLastReturnsToTheChats() {
        let s = split([live("x")], archiveOpen: true)
        XCTAssertEqual(s.pane, .chats)
        XCTAssertNil(s.barCount)
    }

    func testSearchListsArchivedMatchesUnderADivider() {
        let s = split([live("x"), gone("a"), gone("b")], searching: true)
        XCTAssertEqual(s.pane, .chats)
        XCTAssertNil(s.barCount)
        XCTAssertTrue(s.divider)
        XCTAssertEqual(s.top.map(\.id), ["x"])
        XCTAssertEqual(s.archived.map(\.id), ["a", "b"])
        XCTAssertFalse(split([live("x")], searching: true).divider)
    }

    func testSearchInTheArchiveStaysInTheArchive() {
        let s = split([live("x")], searching: true, archiveOpen: true)
        XCTAssertEqual(s.pane, .archive)
        XCTAssertTrue(s.archived.isEmpty)
        XCTAssertFalse(s.divider)
    }
}
