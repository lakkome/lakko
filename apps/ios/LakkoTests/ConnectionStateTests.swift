// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// `docs/ux/connection-state.md` - the one connection-state source.
@MainActor
final class ConnectionStateTests: XCTestCase {

    private func fresh() -> (ConnectionState, () -> [String]) {
        let state = ConnectionState(monitorPath: false)
        var spoken: [String] = []
        state.announce = { spoken.append($0) }
        return (state, { spoken })
    }

    func testANetworkFailureOrNoRouteEntersWaitingFromAnyPhase() {
        for from in [ConnectionPhase.online, .waiting, .connecting] {
            XCTAssertEqual(ConnectionState.next(from, .networkFailure), .waiting)
            XCTAssertEqual(ConnectionState.next(from, .osNoRoute), .waiting)
        }
    }

    func testAnAnswerClearsEveryPhase() {
        for from in [ConnectionPhase.online, .waiting, .connecting] {
            XCTAssertEqual(ConnectionState.next(from, .answered), .online)
        }
    }

    func testTheRouteComingBackMovesWaitingToConnectingAndNothingElse() {
        XCTAssertEqual(ConnectionState.next(.waiting, .osRouteBack), .connecting)
        XCTAssertEqual(ConnectionState.next(.online, .osRouteBack), .online)
        XCTAssertEqual(ConnectionState.next(.connecting, .osRouteBack), .connecting)
    }

    func testOnlyATransportFailureCountsAsANetworkFailure() {
        XCTAssertTrue(ConnectionState.isNetworkFailure(URLError(.notConnectedToInternet)))
        XCTAssertTrue(ConnectionState.isNetworkFailure(URLError(.timedOut)))
        XCTAssertTrue(ConnectionState.isNetworkFailure(URLError(.cannotFindHost)))
        XCTAssertTrue(ConnectionState.isNetworkFailure(URLError(.secureConnectionFailed)))
        XCTAssertTrue(ConnectionState.isNetworkFailure(FfiError.Transport(message: "dial")))
        XCTAssertFalse(ConnectionState.isNetworkFailure(URLError(.cancelled)))
        XCTAssertFalse(ConnectionState.isNetworkFailure(CancellationError()))
        XCTAssertFalse(ConnectionState.isNetworkFailure(URLError(.badURL)))
        XCTAssertFalse(ConnectionState.isNetworkFailure(AntiCensorshipError.badResponse))
    }

    func testConnectingLastsUntilTheFirstAnswerAndAFailedRetryGoesBackToWaiting() {
        let (state, _) = fresh()
        state.notePath(satisfied: true)
        XCTAssertEqual(state.phase, .online)
        state.notePath(satisfied: false)
        XCTAssertEqual(state.phase, .waiting)
        state.notePath(satisfied: true)
        XCTAssertEqual(state.phase, .connecting)
        state.note(.networkFailure)
        XCTAssertEqual(state.phase, .waiting)
        state.note(.answered)
        XCTAssertEqual(state.phase, .online)
    }

    func testARepeatedSatisfiedPathDoesNotLeaveWaiting() {
        let (state, _) = fresh()
        state.notePath(satisfied: true)
        state.note(.networkFailure)
        state.notePath(satisfied: true)
        XCTAssertEqual(state.phase, .waiting, "the simulator keeps a route while the host network is gone")
    }

    func testEachTransitionIsAnnouncedOnce() {
        let (state, spoken) = fresh()
        state.note(.networkFailure)
        state.note(.networkFailure)
        state.note(.answered)
        state.note(.answered)
        XCTAssertEqual(spoken(), [
            Localization.text("connection.waiting"),
            Localization.text("connection.restored_a11y"),
        ])
    }

    func testTheHeaderSwapsToTheConnectionWordsAndBack() {
        XCTAssertNil(ConnectionPhase.online.headerKey)
        XCTAssertEqual(ConnectionPhase.waiting.headerKey, "connection.waiting")
        XCTAssertEqual(ConnectionPhase.connecting.headerKey, "connection.connecting")
        XCTAssertEqual(Localization.text("connection.waiting"), "Waiting for network...")
    }

    func testTheNetworkComingBackIsPosted() {
        let (state, _) = fresh()
        let posted = expectation(forNotification: ConnectionState.networkReturned, object: nil)
        state.note(.osNoRoute)
        state.note(.osRouteBack)
        wait(for: [posted], timeout: 1)
    }
}
