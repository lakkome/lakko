// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// The call that outlived its peer by 45 minutes.
///
/// Seen in the browser on 2026-09-21: a 1x1 call ran for three quarters of an
/// hour after the other side had left it, counting time the whole way. The ICE
/// state never left `connected` — both ends were on one machine, so the
/// candidate pair stayed reachable after the peer stopped taking part. On iOS
/// the same call would have run longer still: `.disconnected` was ignored
/// outright, on the strength of an upper-layer watchdog that does nothing once
/// the state is `.connected`.
final class MediaWatchdogTests: XCTestCase {

    func testFirstSampleStartsTheClock() {
        let a = MediaWatchdog.note(nil, packets: 100, atSec: 5)
        XCTAssertEqual(a, MediaWatchdog.InboundActivity(packets: 100, lastChangeAtSec: 5))
    }

    func testAMovingCountRestartsTheClock() {
        let first = MediaWatchdog.note(nil, packets: 100, atSec: 5)
        let second = MediaWatchdog.note(first, packets: 140, atSec: 7)
        XCTAssertEqual(second, MediaWatchdog.InboundActivity(packets: 140, lastChangeAtSec: 7))
    }

    func testAStandingCountLeavesTheClockAlone() {
        let first = MediaWatchdog.note(nil, packets: 100, atSec: 5)
        let same = MediaWatchdog.note(first, packets: 100, atSec: 9)
        XCTAssertEqual(same.lastChangeAtSec, 5)
    }

    func testACountThatWentBackwardsIsActivityNotSilence() {
        // A reset means the receivers were replaced (renegotiation, ICE
        // restart). Reading it as silence would end a call that is working.
        let first = MediaWatchdog.note(nil, packets: 90_000, atSec: 5)
        let reset = MediaWatchdog.note(first, packets: 12, atSec: 6)
        XCTAssertEqual(reset, MediaWatchdog.InboundActivity(packets: 12, lastChangeAtSec: 6))
    }

    func testSilenceForTheWholeWindowEndsTheCall() {
        let a = MediaWatchdog.note(nil, packets: 100, atSec: 0)
        XCTAssertTrue(MediaWatchdog.hasGoneSilent(a, atSec: 30, timeoutSec: 30))
        XCTAssertFalse(MediaWatchdog.hasGoneSilent(a, atSec: 29.9, timeoutSec: 30))
    }

    func testNothingMeasuredIsNeverSilent() {
        XCTAssertFalse(MediaWatchdog.hasGoneSilent(nil, atSec: 10_000, timeoutSec: 30))
    }

    func testATimeoutOfZeroDisablesItRatherThanFiringInstantly() {
        let a = MediaWatchdog.note(nil, packets: 100, atSec: 0)
        XCTAssertFalse(MediaWatchdog.hasGoneSilent(a, atSec: 10_000, timeoutSec: 0))
        XCTAssertFalse(MediaWatchdog.hasGoneSilent(a, atSec: 10_000, timeoutSec: -1))
    }

    func testATrickleOfMediaKeepsACallAliveIndefinitely() {
        // A muted microphone still sends RTP and a 1x1 call always carries an
        // audio section both ways, so any live call keeps this count moving.
        var a = MediaWatchdog.note(nil, packets: 0, atSec: 0)
        var t: TimeInterval = 2
        while t <= 120 {
            a = MediaWatchdog.note(a, packets: Int64(t), atSec: t)
            XCTAssertFalse(MediaWatchdog.hasGoneSilent(a, atSec: t, timeoutSec: 30))
            t += 2
        }
    }

    func testTheFortyFiveMinuteCallWouldHaveEndedInTheFirstMinute() {
        var a = MediaWatchdog.note(nil, packets: 500_000, atSec: 0)
        var endedAt: TimeInterval?
        var t: TimeInterval = 2
        while t <= 45 * 60 {
            a = MediaWatchdog.note(a, packets: 500_000, atSec: t)
            if endedAt == nil, MediaWatchdog.hasGoneSilent(a, atSec: t, timeoutSec: 30) {
                endedAt = t
            }
            t += 2
        }
        XCTAssertEqual(endedAt, 30)
    }
}
