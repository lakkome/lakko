// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

final class JournalHeadEpochTests: XCTestCase {
    // A "payload" here is one byte: 0 = not MLS, n = a row after which the
    // group is at epoch n. That is exactly the contract of `epochAfterMessage`.
    private func row(_ id: Int64, _ epoch: UInt8) -> JournalHeadEpoch.Row {
        .init(id: id, payload: Data([epoch]))
    }

    private func epochAfter(_ payload: Data) -> UInt64? {
        payload.first.flatMap { $0 == 0 ? nil : UInt64($0) }
    }

    func testPagesToTheHeadInsteadOfTrustingTheFirstPage() async throws {
        var pages: [JournalHeadEpoch.Page] = [
            .init(rows: [row(1, 1), row(2, 0), row(3, 2)], hasMore: true),
            .init(rows: [row(4, 2), row(5, 0)], hasMore: true),
            .init(rows: [row(6, 7)], hasMore: false),
        ]
        var calls: [(Int64, Int)] = []
        let epoch = try await JournalHeadEpoch.read(
            fetchPage: { after, limit in
                calls.append((after, limit))
                return pages.isEmpty ? .init(rows: [], hasMore: false) : pages.removeFirst()
            },
            epochAfter: epochAfter
        )
        XCTAssertEqual(epoch, 7)
        XCTAssertEqual(calls.map(\.0), [0, 3, 5])
        XCTAssertEqual(Set(calls.map(\.1)), [JournalHeadEpoch.pageSize])
    }

    func testKeepsTheLastMlsEpochWhenTheHeadRowsAreNotMls() async throws {
        let epoch = try await JournalHeadEpoch.read(
            fetchPage: { _, _ in .init(rows: [self.row(1, 4), self.row(2, 0), self.row(3, 0)], hasMore: false) },
            epochAfter: epochAfter
        )
        XCTAssertEqual(epoch, 4)
    }

    func testReadsAnEmptyJournalAsEpochZero() async throws {
        let epoch = try await JournalHeadEpoch.read(
            fetchPage: { _, _ in .init(rows: [], hasMore: false) },
            epochAfter: epochAfter
        )
        XCTAssertEqual(epoch, 0)
    }

    func testStopsWhenAPageDoesNotMoveTheCursor() async throws {
        var calls = 0
        let epoch = try await JournalHeadEpoch.read(
            fetchPage: { _, _ in
                calls += 1
                return .init(rows: [self.row(5, 3)], hasMore: true)
            },
            epochAfter: epochAfter
        )
        XCTAssertEqual(epoch, 3)
        XCTAssertEqual(calls, 2)
    }

    func testPropagatesAFetchFailureInsteadOfAnsweringWithAStaleEpoch() async {
        struct Offline: Error {}
        do {
            _ = try await JournalHeadEpoch.read(
                fetchPage: { _, _ in throw Offline() },
                epochAfter: epochAfter
            )
            XCTFail("expected the fetch error to propagate")
        } catch {
            XCTAssertTrue(error is Offline)
        }
    }

    func testScanFindsARowOnALaterPageAndStopsThere() async throws {
        var pages: [JournalHeadEpoch.Page] = [
            .init(rows: [row(1, 1), row(2, 2)], hasMore: true),
            .init(rows: [row(3, 9), row(4, 5)], hasMore: true),
            .init(rows: [row(5, 6)], hasMore: false),
        ]
        var fetched = 0
        let hit: Int64? = try await JournalHeadEpoch.scan(
            fetchPage: { _, _ in
                fetched += 1
                return pages.isEmpty ? .init(rows: [], hasMore: false) : pages.removeFirst()
            },
            visit: { row in row.payload.first == 9 ? row.id : nil }
        )
        XCTAssertEqual(hit, 3)
        XCTAssertEqual(fetched, 2)
    }

    func testScanReturnsNilWhenNothingMatches() async throws {
        let hit: Int64? = try await JournalHeadEpoch.scan(
            fetchPage: { _, _ in .init(rows: [self.row(1, 1)], hasMore: false) },
            visit: { _ in nil }
        )
        XCTAssertNil(hit)
    }
}
