// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The feed's refresh-failed banner rule; mirror of web
/// `feed/feed-refresh.test.ts`.
final class FeedRefreshTests: XCTestCase {
    private func t(_ tried: Int, _ answered: Int) -> FeedCheckTally {
        FeedCheckTally(tried: tried, answered: answered)
    }

    func testClassifiesARun() {
        XCTAssertNil(FeedRefreshState.outcome(of: t(0, 0)))
        XCTAssertEqual(FeedRefreshState.outcome(of: t(3, 0)), .nothing)
        XCTAssertEqual(FeedRefreshState.outcome(of: t(3, 1)), .reached)
    }

    func testOffBeforeAnyCheck() {
        XCTAssertFalse(FeedRefreshState().failed)
    }

    func testShowsWhenNothingOfEitherKindAnswered() {
        let s = FeedRefreshState().recording(.groups, t(2, 0)).recording(.channels, t(1, 0))
        XCTAssertTrue(s.failed)
    }

    func testShowsWhenTheOnlyKindWithSourcesReachedNothing() {
        XCTAssertTrue(FeedRefreshState().recording(.groups, t(2, 0)).failed)
    }

    func testACheckWhereSomeAnsweredIsNotAFailure() {
        XCTAssertFalse(FeedRefreshState().recording(.groups, t(3, 1)).failed)
    }

    func testChannelsReachedMeansNoBannerEvenIfGroupsReachedNothing() {
        let s = FeedRefreshState().recording(.groups, t(2, 0)).recording(.channels, t(1, 1))
        XCTAssertFalse(s.failed)
    }

    func testGoesAtTheNextCheckThatReachesAnything() {
        var s = FeedRefreshState().recording(.groups, t(2, 0))
        XCTAssertTrue(s.failed)
        s = s.recording(.groups, t(2, 1))
        XCTAssertFalse(s.failed)
    }

    func testARunThatTriedNothingChangesNothing() {
        let failed = FeedRefreshState().recording(.groups, t(2, 0))
        XCTAssertEqual(failed.recording(.groups, t(0, 0)), failed)
        let ok = FeedRefreshState().recording(.groups, t(2, 2))
        XCTAssertEqual(ok.recording(.groups, t(0, 0)), ok)
    }
}
