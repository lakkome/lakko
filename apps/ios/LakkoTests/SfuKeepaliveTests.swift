// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The SFU signaling keepalive rule (`SfuKeepalive`), pinned without a socket.
/// The same numbers and the same rule run on Android and web.
final class SfuKeepaliveTests: XCTestCase {

    func testTheFirstPingGoesOutOneIntervalAfterTheClockStarts() {
        XCTAssertEqual(SfuKeepalive.action(nowMs: 15_000, lastInboundMs: 0, lastPingMs: 0), .ping)
        XCTAssertEqual(SfuKeepalive.action(nowMs: 14_999, lastInboundMs: 0, lastPingMs: 0), .wait)
    }

    func testAPingIsDueOnlyOneIntervalAfterTheLastOne() {
        XCTAssertEqual(SfuKeepalive.action(nowMs: 20_000, lastInboundMs: 16_000, lastPingMs: 15_000), .wait)
        XCTAssertEqual(SfuKeepalive.action(nowMs: 30_000, lastInboundMs: 16_000, lastPingMs: 15_000), .ping)
    }

    func testTheDeadlineWinsOverADuePing() {
        // Three pings unanswered: dead, not a fourth ping.
        XCTAssertEqual(SfuKeepalive.action(nowMs: 45_000, lastInboundMs: 0, lastPingMs: 30_000), .dead)
        XCTAssertEqual(SfuKeepalive.action(nowMs: 44_999, lastInboundMs: 0, lastPingMs: 30_000), .wait)
    }

    func testAnInboundFrameResetsTheDeadline() {
        // A Pong at 40 s keeps a link alive that would otherwise be dead at 45 s.
        XCTAssertEqual(SfuKeepalive.action(nowMs: 50_000, lastInboundMs: 40_000, lastPingMs: 45_000), .wait)
        XCTAssertEqual(SfuKeepalive.action(nowMs: 85_000, lastInboundMs: 40_000, lastPingMs: 75_000), .dead)
    }

    func testTheConstantsAreTheSharedOnes() {
        XCTAssertEqual(SfuKeepalive.pingIntervalMs, 15_000)
        XCTAssertEqual(SfuKeepalive.deadlineMs, 45_000)
        XCTAssertEqual(SfuKeepalive.tickMs, 5_000)
    }

    // MARK: - When a call ended

    func testACallEndsAtTheClockWhileTheLinkWasAliveWithinTheDeadline() {
        XCTAssertEqual(SfuKeepalive.callEndedAtMs(nowMs: 100_000, lastInboundMs: 90_000), 100_000)
        XCTAssertEqual(
            SfuKeepalive.callEndedAtMs(nowMs: 100_000, lastInboundMs: 100_000 - SfuKeepalive.deadlineMs),
            100_000)
    }

    /// The overnight case: the laptop slept at 22:58 and woke at 07:16; the
    /// row must not say 8:20:53.
    func testACallEndsAtTheLastProofOfLifeOnceTheDeadlineHasPassed() {
        let slept: Int64 = 100_000
        let woke = slept + 8 * 3_600_000
        XCTAssertEqual(SfuKeepalive.callEndedAtMs(nowMs: woke, lastInboundMs: slept), slept)
        let edge = 100_000 - SfuKeepalive.deadlineMs - 1
        XCTAssertEqual(SfuKeepalive.callEndedAtMs(nowMs: 100_000, lastInboundMs: edge), edge)
    }

    func testACallWhoseLinkNeverCameUpEndsAtTheClock() {
        XCTAssertEqual(SfuKeepalive.callEndedAtMs(nowMs: 100_000, lastInboundMs: 0), 100_000)
    }
}
