// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// When Lakko's own full-screen incoming call is on screen
/// (`docs/ux/calls.md` section 2.2), and — the constraint that matters most —
/// when it must stay off: because the phone's own call screen owns the call, or
/// because the conversation is muted and the call is being offered quietly
/// instead (section 2.1.1).
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

    // MARK: - A muted conversation (docs/ux/calls.md section 2.1.1)

    /// The rule this file exists to pin, in its muted form: a call the person
    /// asked not to be interrupted by is answerable, but neither screen that
    /// takes over may open for it.
    ///
    /// **What changed.** A muted call used to reach neither branch, because it
    /// opened no session at all — which is what made it unanswerable. Now it
    /// rings like any other call and this is the one thing that differs.
    func testAMutedConversationShowsNeitherScreenThatTakesOver() {
        XCTAssertEqual(
            IncomingCallPresentation.decide(
                source: .signalingRow, appIsActive: true, alreadyReported: false, quiet: true),
            .quiet)
        XCTAssertEqual(
            IncomingCallPresentation.decide(
                source: .signalingRow, appIsActive: false, alreadyReported: false, quiet: true),
            .quiet,
            "a backgrounded app must not ring the phone for a muted conversation either")
        XCTAssertFalse(
            IncomingCallPresentation.showsInAppScreen(
                state: .ringing, ringingOnSystemUi: false, quiet: true))
    }

    /// `.quiet` is not `.none`. Both report nothing, but one means the call is
    /// already ringing elsewhere and the other means it is being offered, in
    /// the layout, on two surfaces this rule knows nothing about.
    func testQuietIsADistinctVerdictFromNothingToDo() {
        XCTAssertNotEqual(IncomingCallUi.quiet, IncomingCallUi.none)
    }

    /// A VoIP push reports whatever the mute flag says, because iOS terminates
    /// an app that takes one without reporting a call. The muted case is
    /// handled by ending that call at once, never by skipping the report.
    func testAVoipPushStillReportsForAMutedConversation() {
        XCTAssertEqual(
            IncomingCallPresentation.decide(
                source: .voipPush, appIsActive: false, alreadyReported: false, quiet: true),
            .systemCallUi)
    }

    /// A call the phone is ALREADY ringing keeps its surface. That read happened
    /// on a path which consulted the same mute flag and chose to ring — which is
    /// section 2.1's own fallback for a flag it could not resolve — and
    /// answering `.quiet` here would leave CallKit showing a call with nothing
    /// left to update it.
    func testACallAlreadyOnThePhoneIsNotTakenOverByTheQuietVerdict() {
        XCTAssertEqual(
            IncomingCallPresentation.decide(
                source: .signalingRow, appIsActive: true, alreadyReported: true, quiet: true),
            .none)
    }

    /// Both defaults fail toward ringing — the direction section 2.1 requires,
    /// since an unwanted ring is an annoyance and a swallowed call is one the
    /// person never learns about.
    func testAnUnansweredMuteQuestionRings() {
        XCTAssertEqual(
            IncomingCallPresentation.decide(
                source: .signalingRow, appIsActive: true, alreadyReported: false),
            .inApp)
        XCTAssertTrue(
            IncomingCallPresentation.showsInAppScreen(
                state: .ringing, ringingOnSystemUi: false))
    }

    // MARK: - The quiet surfaces themselves

    /// Neither quiet surface draws for anything but a ringing, muted call.
    func testTheQuietOfferExistsOnlyForARingingMutedCall() {
        let states: [CallSession.WebCallState?] = [
            nil, .idle, .outgoing, .connecting, .connected, .ending, .done,
        ]
        for state in states {
            XCTAssertNil(
                QuietIncomingCallPresentation.offer(
                    state: state, quiet: true, groupIdHex: "aa11",
                    callerName: "Alice", video: false),
                "state \(String(describing: state))")
        }
        XCTAssertNil(
            QuietIncomingCallPresentation.offer(
                state: .ringing, quiet: false, groupIdHex: "aa11",
                callerName: "Alice", video: false),
            "an unmuted ring has the full-screen surfaces and must not also be offered quietly")
        XCTAssertNil(
            QuietIncomingCallPresentation.offer(
                state: .ringing, quiet: true, groupIdHex: nil,
                callerName: "Alice", video: false),
            "a call with no conversation has no surface to be offered on")
        XCTAssertNotNil(
            QuietIncomingCallPresentation.offer(
                state: .ringing, quiet: true, groupIdHex: "aa11",
                callerName: "Alice", video: false))
    }

    /// The group id reaches the two surfaces from different places — a journal
    /// row's own hex on one side, a stored thread id on the other — and nothing
    /// guarantees they agree on case. The mute mirror's key is folded for the
    /// same reason.
    func testTheConversationMatchIsCaseInsensitive() {
        let quiet = QuietIncomingCallPresentation.offer(
            state: .ringing, quiet: true, groupIdHex: "AA11", callerName: nil, video: false)
        XCTAssertTrue(
            QuietIncomingCallPresentation.showsInConversation(quiet, openGroupIdHex: "aa11"))
        XCTAssertTrue(QuietIncomingCallPresentation.showsOnListRow(quiet, rowId: "aa11"))
    }

    /// Nothing is offered when there is nothing on offer — the ordinary state,
    /// and the one both views spend their lives in.
    func testNoOfferDrawsNothingAnywhere() {
        XCTAssertFalse(
            QuietIncomingCallPresentation.showsInConversation(nil, openGroupIdHex: "aa11"))
        XCTAssertFalse(QuietIncomingCallPresentation.showsOnListRow(nil, rowId: "aa11"))
    }

    /// The word the person reads comes from the offer's own media flag, and a
    /// caller nothing is known about gets the placeholder rather than a blank.
    func testTheQuietOfferSaysWhatKindOfCallItIs() {
        let video = QuietIncomingCall(groupIdHex: "aa11", callerName: "Alice", video: true)
        XCTAssertEqual(video.kindLabel, "Video call")
        XCTAssertEqual(video.displayName, "Alice")
        let voice = QuietIncomingCall(groupIdHex: "aa11", callerName: nil, video: false)
        XCTAssertEqual(voice.kindLabel, "Voice call")
        XCTAssertEqual(voice.displayName, CallPeerName.placeholder)
        XCTAssertEqual(
            l10nPlain(QuietIncomingCallPresentation.spokenSummary(video)), "Video call from Alice")
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
