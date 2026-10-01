// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// The rule that notices a forked DeviceGroup (``DeviceGroupFork``): drained
/// and behind the DS for longer than the grace.
final class DeviceGroupForkTests: XCTestCase {

    private let t0: Int64 = 1_800_000_000_000

    // MARK: - In sync

    func testNoLatestEpochIsInSync() {
        XCTAssertEqual(
            DeviceGroupFork.verdict(
                localEpoch: 3, drained: true, latestEpoch: nil, suspectSince: nil, now: t0),
            .inSync)
    }

    func testLatestEqualToLocalIsInSync() {
        XCTAssertEqual(
            DeviceGroupFork.verdict(
                localEpoch: 3, drained: true, latestEpoch: 3, suspectSince: nil, now: t0),
            .inSync)
    }

    func testLatestBelowLocalIsInSync() {
        XCTAssertEqual(
            DeviceGroupFork.verdict(
                localEpoch: 4, drained: true, latestEpoch: 3, suspectSince: nil, now: t0),
            .inSync)
    }

    /// A fetch that returned rows is never drained, even when none applied,
    /// however far behind the DS says this device is.
    func testFetchWithRowsIsNeverDrained() {
        XCTAssertEqual(
            DeviceGroupFork.verdict(
                localEpoch: 3, drained: false, latestEpoch: 9,
                suspectSince: t0, now: t0 + DeviceGroupFork.graceMs * 10),
            .inSync)
    }

    // MARK: - The timer

    func testFirstBehindPollStartsTheTimer() {
        XCTAssertEqual(
            DeviceGroupFork.verdict(
                localEpoch: 3, drained: true, latestEpoch: 4, suspectSince: nil, now: t0),
            .suspect(since: t0))
    }

    func testWithinTheGraceKeepsTheOriginalStart() {
        XCTAssertEqual(
            DeviceGroupFork.verdict(
                localEpoch: 3, drained: true, latestEpoch: 4,
                suspectSince: t0, now: t0 + DeviceGroupFork.graceMs),
            .suspect(since: t0))
    }

    func testPastTheGraceIsForked() {
        XCTAssertEqual(
            DeviceGroupFork.verdict(
                localEpoch: 3, drained: true, latestEpoch: 4,
                suspectSince: t0, now: t0 + DeviceGroupFork.graceMs + 1),
            .forked)
    }

    /// Walks the poll sequence the sync loop drives: a suspicion is cleared
    /// by any in-sync poll (here the commit arrived as a row), so a later
    /// behind poll starts a fresh timer rather than inheriting the old one.
    func testAnyInSyncPollClearsTheTimer() {
        var since: Int64?
        func step(local: UInt64, drained: Bool, latest: UInt64?, at now: Int64) -> DeviceGroupFork.Verdict {
            let v = DeviceGroupFork.verdict(
                localEpoch: local, drained: drained, latestEpoch: latest,
                suspectSince: since, now: now)
            if case .suspect(let s) = v { since = s } else { since = nil }
            return v
        }
        XCTAssertEqual(step(local: 3, drained: true, latest: 4, at: t0), .suspect(since: t0))
        XCTAssertEqual(step(local: 3, drained: false, latest: 4, at: t0 + 1_000), .inSync)
        XCTAssertNil(since)
        let later = t0 + DeviceGroupFork.graceMs * 2
        XCTAssertEqual(step(local: 4, drained: true, latest: 5, at: later), .suspect(since: later))
        XCTAssertEqual(
            step(local: 4, drained: true, latest: 5, at: later + DeviceGroupFork.graceMs),
            .suspect(since: later))
        XCTAssertEqual(
            step(local: 4, drained: true, latest: 5, at: later + DeviceGroupFork.graceMs + 1),
            .forked)
    }

    func testGraceIsTwoMinutes() {
        XCTAssertEqual(DeviceGroupFork.graceMs, 120_000)
    }

    // MARK: - The fetch result carries the DS's epoch

    func testFetchResultDefaultsToNoLatestEpoch() {
        XCTAssertNil(FetchResult(messages: [], hasMore: false).latestEpoch)
        XCTAssertEqual(FetchResult(messages: [], hasMore: false, latestEpoch: 7).latestEpoch, 7)
    }

    /// A claimed journal's read carries `latest_epoch`; an unclaimed one does not.
    func testFetchGroupSurfacesLatestEpoch() async throws {
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [
            .init(status: 200, body: Data(#"{"messages":[],"has_more":false,"latest_epoch":12}"#.utf8)),
            .init(status: 200, body: Data(#"{"messages":[],"has_more":false}"#.utf8)),
        ])
        let ds = DsAccessURLProtocol.client(scope)
        let groupId = Data(repeating: 0x5a, count: 32)
        let claimed = try await ds.fetchGroup(
            groupId: groupId, after: 0, limit: nil, waitSecs: nil, auth: nil)
        XCTAssertEqual(claimed.latestEpoch, 12)
        let unclaimed = try await ds.fetchGroup(
            groupId: groupId, after: 0, limit: nil, waitSecs: nil, auth: nil)
        XCTAssertNil(unclaimed.latestEpoch)
    }
}
