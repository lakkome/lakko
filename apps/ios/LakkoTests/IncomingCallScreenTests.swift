// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// When Lakko's own full-screen incoming call is on screen
/// (`docs/ux/calls.md` section 2.2), and — the constraint that matters most —
/// when it must stay off because the phone's own call screen owns the call.
@MainActor
final class IncomingCallScreenTests: XCTestCase {

    // MARK: - The in-app screen appears for a ringing call

    func testARingingCallShowsTheInAppScreen() {
        XCTAssertTrue(
            IncomingCallPresentation.showsInAppScreen(
                state: .ringing, ringingOnSystemUi: false))
    }

    /// Nothing else does. A call that is connecting, live or over has the call
    /// screen, not the incoming one.
    func testNoOtherStateShowsIt() {
        let others: [CallSession.WebCallState?] = [
            nil, .idle, .outgoing, .connecting, .connected, .ending, .done,
        ]
        for state in others {
            XCTAssertFalse(
                IncomingCallPresentation.showsInAppScreen(
                    state: state, ringingOnSystemUi: false),
                "state \(String(describing: state))")
        }
    }

    // MARK: - It must never compete with the system call screen

    /// The constraint: when CallKit is showing the call, Lakko draws nothing.
    /// Two surfaces for one call is what section 2.1 forbids, and it is the
    /// failure a view can reach on its own — `decide` runs once when the ring
    /// starts, while the view re-evaluates for as long as it rings.
    func testACallOnTheSystemScreenShowsNoInAppScreen() {
        XCTAssertFalse(
            IncomingCallPresentation.showsInAppScreen(
                state: .ringing, ringingOnSystemUi: true))
    }

    /// The two surfaces are exclusive for every state, not only while ringing.
    func testTheTwoSurfacesAreNeverBothOnScreen() {
        let states: [CallSession.WebCallState?] = [
            nil, .idle, .ringing, .outgoing, .connecting, .connected, .ending, .done,
        ]
        for state in states {
            XCTAssertFalse(
                IncomingCallPresentation.showsInAppScreen(
                    state: state, ringingOnSystemUi: true),
                "state \(String(describing: state)) must yield to the system screen")
        }
    }

    // MARK: - The surface the ring chose is what the view honours

    /// `decide` picks one surface per call; these two assertions pin that the
    /// view's rule agrees with it, so the choice cannot be made twice with
    /// different answers.
    func testAnInAppDecisionIsWhatTheViewDraws() {
        let surface = IncomingCallPresentation.decide(
            source: .signalingRow, appIsActive: true, alreadyReported: false)
        XCTAssertEqual(surface, .inApp)
        XCTAssertTrue(
            IncomingCallPresentation.showsInAppScreen(
                state: .ringing, ringingOnSystemUi: surface != .inApp))
    }

    func testASystemDecisionKeepsTheViewQuiet() {
        for surface in [
            IncomingCallPresentation.decide(
                source: .voipPush, appIsActive: false, alreadyReported: false),
            IncomingCallPresentation.decide(
                source: .signalingRow, appIsActive: false, alreadyReported: false),
            IncomingCallPresentation.decide(
                source: .signalingRow, appIsActive: true, alreadyReported: true),
        ] {
            XCTAssertNotEqual(surface, .inApp)
            XCTAssertFalse(
                IncomingCallPresentation.showsInAppScreen(
                    state: .ringing, ringingOnSystemUi: surface != .inApp),
                "surface \(surface)")
        }
    }

    // MARK: - What the callee is told

    /// The word on the screen comes from the offer's own media flags, so it
    /// cannot disagree with the call that was placed (section 3).
    func testTheKindMatchesTheOffer() {
        XCTAssertEqual(CallMediaChoice.kindLabel(video: false), "Voice call")
        XCTAssertEqual(CallMediaChoice.kindLabel(video: true), "Video call")
    }

    /// A caller nothing is known about is never labelled "Unknown".
    func testANamelessCallerGetsThePlaceholderNotUnknown() {
        XCTAssertNil(CallPeerName.resolve(profileName: nil, chatTitle: nil))
        XCTAssertNotEqual(CallPeerName.placeholder, "Unknown")
    }
}
