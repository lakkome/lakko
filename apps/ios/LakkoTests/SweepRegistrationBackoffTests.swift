// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// Live walk 2026-09-29 — the retry schedule of a sweep whose epoch
/// registration the DS refused: 30 s, doubling to at most 10 minutes, reset by
/// an epoch change (a membership or role change is a commit).
final class SweepRegistrationBackoffTests: XCTestCase {

    func testTheDelayDoublesFromThirtySecondsToTenMinutes() {
        XCTAssertEqual(SweepRegistrationBackoff.delayMs(failures: 0), 0)
        XCTAssertEqual(SweepRegistrationBackoff.delayMs(failures: 1), 30_000)
        XCTAssertEqual(SweepRegistrationBackoff.delayMs(failures: 2), 60_000)
        XCTAssertEqual(SweepRegistrationBackoff.delayMs(failures: 3), 120_000)
        XCTAssertEqual(SweepRegistrationBackoff.delayMs(failures: 4), 240_000)
        XCTAssertEqual(SweepRegistrationBackoff.delayMs(failures: 5), 480_000)
        XCTAssertEqual(SweepRegistrationBackoff.delayMs(failures: 6), 600_000)
        XCTAssertEqual(SweepRegistrationBackoff.delayMs(failures: 1_000), 600_000)
    }

    func testConsecutiveRefusalsAtOneEpochCount() {
        let first = SweepRegistrationBackoff.afterRefusal(nil, nowMs: 1_000, epoch: 4)
        XCTAssertEqual(first.failures, 1)
        XCTAssertEqual(first.notBeforeMs, 31_000)
        let second = SweepRegistrationBackoff.afterRefusal(first, nowMs: 31_000, epoch: 4)
        XCTAssertEqual(second.failures, 2)
        XCTAssertEqual(second.notBeforeMs, 91_000)
    }

    func testARefusalAtANewEpochCountsFromOne() {
        let old = SweepRegistrationBackoff(failures: 5, epoch: 4, failedAtMs: 0)
        let next = SweepRegistrationBackoff.afterRefusal(old, nowMs: 1_000, epoch: 5)
        XCTAssertEqual(next.failures, 1)
        XCTAssertEqual(next.epoch, 5)
    }

    func testTheWaitHoldsUntilItRunsOut() {
        let backoff = SweepRegistrationBackoff.afterRefusal(nil, nowMs: 1_000, epoch: 4)
        XCTAssertFalse(backoff.allows(nowMs: 1_000, epoch: 4))
        XCTAssertFalse(backoff.allows(nowMs: 30_999, epoch: 4))
        XCTAssertTrue(backoff.allows(nowMs: 31_000, epoch: 4))
    }

    func testAnEpochChangeEndsTheWait() {
        let backoff = SweepRegistrationBackoff.afterRefusal(nil, nowMs: 1_000, epoch: 4)
        XCTAssertTrue(backoff.allows(nowMs: 1_001, epoch: 5), "a membership or role change moved the epoch")
    }

    func testAClockThatWentBackwardsDoesNotHoldForever() {
        let backoff = SweepRegistrationBackoff(failures: 6, epoch: 4, failedAtMs: 1_000_000)
        XCTAssertTrue(backoff.allows(nowMs: 999_999, epoch: 4))
    }

    func testOnlyTheRefusalsTheDsRepeatsAreBackedOff() {
        XCTAssertTrue(SweepRegistrationBackoff.appliesTo(DsStatementError.unauthorized))
        XCTAssertTrue(SweepRegistrationBackoff.appliesTo(DsStatementError.malformed("x")))
        XCTAssertTrue(SweepRegistrationBackoff.appliesTo(DsStatementError.rankShortfall))
        XCTAssertFalse(SweepRegistrationBackoff.appliesTo(DsStatementError.conflict))
        XCTAssertFalse(SweepRegistrationBackoff.appliesTo(DsStatementError.http(status: 503, body: "")))
        XCTAssertFalse(SweepRegistrationBackoff.appliesTo(URLError(.timedOut)))
    }
}
