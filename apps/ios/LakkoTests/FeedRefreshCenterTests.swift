// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The app-wide holder of the Feed's "Couldn't check for new messages."
/// state: per account, fed by the background group sweep as well as by pull
/// to refresh (`docs/ux/feed.md`, row "refresh failed (network)").
@MainActor
final class FeedRefreshCenterTests: XCTestCase {

    private final class Box {
        var failing: Set<String> = []
        var reported: [FeedCheckTally] = []
    }

    private func t(_ tried: Int, _ answered: Int) -> FeedCheckTally {
        FeedCheckTally(tried: tried, answered: answered)
    }

    func testNothingIsRecordedWhileNoAccountIsBound() {
        let center = FeedRefreshCenter()
        center.record(.groups, t(2, 0))
        XCTAssertFalse(center.failed)
        XCTAssertEqual(center.state, FeedRefreshState())
    }

    func testAFailedCheckRaisesTheBannerAndTheNextReachingCheckClearsIt() {
        let center = FeedRefreshCenter()
        center.bind(accountIdB64: "A")
        center.record(.groups, t(3, 0))
        XCTAssertTrue(center.failed)
        center.record(.channels, t(0, 0))
        XCTAssertTrue(center.failed, "a check that tried nothing leaves it as it was")
        center.record(.channels, t(2, 1))
        XCTAssertFalse(center.failed, "a channel that answered clears it")
    }

    func testRebindingTheSameAccountKeepsTheStateAnotherAccountForgetsIt() {
        let center = FeedRefreshCenter()
        center.bind(accountIdB64: "A")
        center.record(.groups, t(1, 0))
        center.bind(accountIdB64: "A")
        XCTAssertTrue(center.failed)
        center.bind(accountIdB64: "B")
        XCTAssertFalse(center.failed)
        XCTAssertEqual(center.state, FeedRefreshState())
    }

    func testResetForgetsAndUnbinds() {
        let center = FeedRefreshCenter()
        center.bind(accountIdB64: "A")
        center.record(.groups, t(1, 0))
        center.reset()
        XCTAssertFalse(center.failed)
        XCTAssertNil(center.accountIdB64)
        center.record(.groups, t(1, 0))
        XCTAssertFalse(center.failed, "after sign-out a late check reports nothing")
    }

    func testABackgroundSweepTallyChangesTheHolder() async {
        let center = FeedRefreshCenter()
        center.bind(accountIdB64: "A")
        let box = Box()
        box.failing = ["a", "b"]
        let scheduler = ForegroundReceiveScheduler(
            ports: .init(
                groups: { ["a", "b"] },
                isExcluded: { _ in false },
                poll: { group, _ in
                    if box.failing.contains(group) { throw URLError(.notConnectedToInternet) }
                    return false
                },
                callGroup: { nil },
                sleepMs: { _ in },
                random: { 0.5 },
                nowMs: { 0 },
                onSweepDone: { tally in
                    box.reported.append(tally)
                    center.record(.groups, tally)
                }
            ),
            config: .init(concurrency: 1, backoffBaseMs: 0, backoffMaxMs: 0)
        )
        scheduler.setForeground(true)

        await scheduler.runSweep()
        XCTAssertEqual(box.reported.last, t(2, 0))
        XCTAssertTrue(center.failed, "a sweep that reached no group raises the banner")

        box.failing = ["b"]
        await scheduler.runSweep()
        XCTAssertEqual(box.reported.last, t(2, 1))
        XCTAssertFalse(center.failed, "the next sweep that reaches a group clears it")
    }

    func testASweepWithNothingToPollReportsAnEmptyTally() async {
        let box = Box()
        let scheduler = ForegroundReceiveScheduler(
            ports: .init(
                groups: { ["open"] },
                isExcluded: { _ in true },
                poll: { _, _ in false },
                callGroup: { nil },
                sleepMs: { _ in },
                random: { 0.5 },
                nowMs: { 0 },
                onSweepDone: { box.reported.append($0) }
            ),
            config: .init(concurrency: 1)
        )
        scheduler.setForeground(true)
        await scheduler.runSweep()
        XCTAssertEqual(box.reported, [t(0, 0)])
    }
}
