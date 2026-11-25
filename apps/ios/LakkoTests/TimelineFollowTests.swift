// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The keyboard or a bar taking room keeps a conversation at its newest
/// message only when it was there before (`TimelineFollow`).
final class TimelineFollowTests: XCTestCase {

    func testAtTheNewestMessageTheKeyboardKeepsItThere() {
        // The defect seen 2026-11-25: the frame ended at 506.7 pt with the
        // keyboard up; the conversation had been at its newest message.
        XCTAssertTrue(TimelineFollow.staysAtBottom(
            contentMaxY: 700, oldViewportHeight: 700, newViewportHeight: 506.7))
    }

    func testScrolledUpKeepsItsPlace() {
        // Content ends 900 pt below the frame: the person is reading older
        // messages.
        XCTAssertFalse(TimelineFollow.staysAtBottom(
            contentMaxY: 1600, oldViewportHeight: 700, newViewportHeight: 506.7))
    }

    func testTheKeyboardGoingAwayAtTheBottomStaysThere() {
        XCTAssertTrue(TimelineFollow.staysAtBottom(
            contentMaxY: 506.7, oldViewportHeight: 506.7, newViewportHeight: 700))
    }

    func testAPinnedBarAppearingAtTheBottomStaysThere() {
        XCTAssertTrue(TimelineFollow.staysAtBottom(
            contentMaxY: 640, oldViewportHeight: 640, newViewportHeight: 588))
    }

    func testAFewPointsShortOfTheEndStillCountsAsAtTheBottom() {
        XCTAssertTrue(TimelineFollow.isAtBottom(contentMaxY: 700 + 20, viewportHeight: 700))
        XCTAssertFalse(TimelineFollow.isAtBottom(contentMaxY: 700 + 30, viewportHeight: 700))
    }

    func testAShortConversationIsAlwaysAtTheBottom() {
        XCTAssertTrue(TimelineFollow.isAtBottom(contentMaxY: 120, viewportHeight: 700))
    }

    func testTheFirstMeasurementAndNoChangeAreNotAChange() {
        XCTAssertFalse(TimelineFollow.staysAtBottom(
            contentMaxY: 700, oldViewportHeight: 0, newViewportHeight: 700))
        XCTAssertFalse(TimelineFollow.staysAtBottom(
            contentMaxY: 700, oldViewportHeight: 700, newViewportHeight: 700.2))
    }

    func testUnmeasuredValuesNeverCountAsAtTheBottom() {
        XCTAssertFalse(TimelineFollow.isAtBottom(contentMaxY: .nan, viewportHeight: 700))
        XCTAssertFalse(TimelineFollow.isAtBottom(contentMaxY: 100, viewportHeight: 0))
    }
}
