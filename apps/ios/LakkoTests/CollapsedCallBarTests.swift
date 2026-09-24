// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// `docs/ux/calls.md` section 4.5 — putting a call away without ending it, as
/// values.
///
/// ## What can be tested here and what cannot
///
/// This client has no XCUITest target, and a SwiftUI body is unreachable from
/// XCTest even with one, so nothing below asserts how the bar LOOKS or that a
/// tap reaches it. What is testable is everything that was lifted out of the
/// view on purpose: which phase the bar sees, whether a collapse outlives the
/// call, when the running time started, the words on the line, and whether a
/// finished drag was the swipe.
///
/// Three things are therefore NOT covered by any test and are covered by
/// structure instead: that the bar is drawn in the top inset above every screen,
/// that the call screen's `onDisappear` skips `state.reset()` during a collapse,
/// and that the mute circle's touch target is 44 pt.
@MainActor
final class CollapsedCallBarTests: XCTestCase {

    /// Every phase, so a rule that grew a case cannot be tested on a subset.
    private let everyPhase: [CallBarPhase] = [
        .idle, .ringing, .preparing, .calling, .connecting, .connected,
        .reconnecting, .ended,
    ]

    // MARK: - Which phase the bar sees (1x1)

    /// The reason ``CallBarPresentation/phase(oneToOne:)`` exists instead of
    /// reusing `CallUiState.mapSessionPhase`.
    ///
    /// That mapping folds `.ringing` into `Calling`, which is safe for the call
    /// screen — a screen only exists once the ring is over — and would put a bar
    /// on screen for a call that is only being offered. The case that makes it
    /// reachable is a MUTED conversation (section 2.1.1): the ring is deliberately
    /// given no screen at all, so `callScreenUp` is false and the phase is the
    /// only thing left to keep the bar away.
    func testARingingOneToOneCallIsRingingAndNotCalling() {
        XCTAssertEqual(CallBarPresentation.phase(oneToOne: .ringing), .ringing)
        XCTAssertFalse(
            callBarVisible(phase: CallBarPresentation.phase(oneToOne: .ringing), callScreenUp: false),
            "a call that is only ringing in must not collapse into a bar")
    }

    /// An outgoing call collapses from the moment it is placed, and the bar reads
    /// `Calling...` — so the dial has to reach the bar as `Calling`, not as the
    /// instant before it.
    func testAnOutgoingOneToOneCallIsCalling() {
        XCTAssertEqual(CallBarPresentation.phase(oneToOne: .outgoing), .calling)
        XCTAssertTrue(
            callBarVisible(phase: .calling, callScreenUp: false))
    }

    /// A call being hung up is still a call. The screen's own status line keeps
    /// saying the running time through `.ending`; a bar that vanished at the
    /// first frame of a hang-up would drop the app's layout and then put it back.
    func testACallBeingHungUpStillHasABar() {
        XCTAssertEqual(CallBarPresentation.phase(oneToOne: .ending), .connected)
        XCTAssertTrue(callBarVisible(phase: .connected, callScreenUp: false))
    }

    func testAFinishedOrAbsentOneToOneCallHasNoBar() {
        XCTAssertEqual(CallBarPresentation.phase(oneToOne: .done), .ended)
        XCTAssertEqual(CallBarPresentation.phase(oneToOne: nil), .idle)
        for phase in [CallBarPhase.ended, .idle] {
            XCTAssertFalse(callBarVisible(phase: phase, callScreenUp: false))
        }
    }

    // MARK: - Which phase the bar sees (group / link)

    /// The external mapping is an identity across the six cases a `CallUiState`
    /// has. Written out rather than assumed, because the two enums are free to
    /// diverge and only this test would notice.
    func testTheExternalPhaseMappingCoversEveryCase() {
        XCTAssertEqual(CallBarPresentation.phase(external: .preparing), .preparing)
        XCTAssertEqual(CallBarPresentation.phase(external: .calling), .calling)
        XCTAssertEqual(CallBarPresentation.phase(external: .connecting), .connecting)
        XCTAssertEqual(CallBarPresentation.phase(external: .connected), .connected)
        XCTAssertEqual(
            CallBarPresentation.phase(external: .reconnecting(reason: "ice")), .reconnecting)
        XCTAssertEqual(
            CallBarPresentation.phase(external: .ended(endedAt: Date())), .ended)
    }

    // MARK: - How long a collapse lasts

    /// A collapse outlives exactly the phases the bar is shown in. Derived from
    /// the Rust rule rather than restated, and this pins the derivation so a
    /// second hand-written list cannot reappear.
    func testACollapseOutlivesExactlyTheLiveCallPhases() {
        for phase in everyPhase {
            XCTAssertEqual(
                CallBarPresentation.collapseSurvives(phase),
                callBarVisible(phase: phase, callScreenUp: false),
                "\(phase) disagreed with the rule about whether a collapse outlives it")
        }
    }

    /// The bar exists because the screen is away. Fed the client's own read of
    /// its presentation, the rule must hide the bar for every phase — the iOS
    /// side of "one state, one place".
    func testNoPhasePutsABarOverTheCallScreen() {
        for phase in everyPhase {
            XCTAssertFalse(
                callBarVisible(phase: phase, callScreenUp: true),
                "\(phase) put a bar over the call screen")
        }
    }

    // MARK: - The running time

    /// The defect this closed: the moment media started flowing was view state on
    /// the call screen, so a call put away for two minutes and brought back read
    /// `0:00` while the bar a second earlier had read `2:07`. Section 4.5 says
    /// collapsing leaves the timer untouched, so the moment is the call's.
    ///
    /// The two things asserted are the two ways it could be lost: putting the
    /// call away and bringing it back must not touch it, and folding `Connected`
    /// again must not restart it. The second is what keeps the fold safe to run
    /// more than once per call — it is driven by a phase change today, and a
    /// caller that ran it on every poll instead would otherwise reset the clock
    /// to zero on every tick without failing anything else.
    func testTheRunningTimeSurvivesACollapseAndComingBack() {
        let bar = CallBarHost()
        let connected = Date(timeIntervalSince1970: 1_000)
        bar.observe(phase: .connected, now: connected)
        XCTAssertEqual(bar.connectedAt, connected)

        bar.collapse()
        XCTAssertTrue(bar.collapsed)
        // Two minutes of the app being used, then back to the call.
        bar.expand()
        XCTAssertEqual(
            bar.connectedAt, connected,
            "the call's own connect moment was replaced while it was put away")

        bar.observe(phase: .connected, now: connected.addingTimeInterval(120))
        XCTAssertEqual(
            bar.connectedAt, connected,
            "folding `Connected` again restarted the running time")
    }

    /// Only a connected call has a running time, and a phase that is not
    /// connected drops it. This is the call screen's existing behaviour, kept
    /// rather than quietly changed: a reconnect restarts the clock, and the line
    /// says `Reconnecting...` while it is gone, so nothing shows a time the
    /// screen would disagree with.
    func testOnlyAConnectedCallHasARunningTime() {
        let now = Date(timeIntervalSince1970: 1_000)
        for phase in everyPhase where phase != .connected {
            XCTAssertNil(
                CallBarPresentation.connectedAt(previous: now, phase: phase, now: now),
                "\(phase) kept a running time")
        }
        XCTAssertEqual(
            CallBarPresentation.connectedAt(previous: nil, phase: .connected, now: now), now)
    }

    // MARK: - A collapsed call that ends

    /// Section 4.5: the `Call ended` overlay belongs to the call screen, and "a
    /// person who put the call away asked not to be interrupted by it". So the
    /// end of a collapsed call is reported, and the caller drops the presentation
    /// instead of letting it spring back over whatever was being read.
    ///
    /// It also cannot simply be left alone: nothing would then dismiss an overlay
    /// that was never drawn, and the host would hold a finished call all session.
    func testACollapsedCallThatEndsIsReportedSoItsScreenIsDropped() {
        let bar = CallBarHost()
        bar.observe(phase: .connected, now: Date())
        bar.collapse()

        XCTAssertTrue(
            bar.observe(phase: .ended, now: Date()),
            "the end of a collapsed call was not reported")
        XCTAssertFalse(bar.collapsed)
        XCTAssertNil(bar.connectedAt)
    }

    /// A call that ended on its own screen needs nothing: that screen's own
    /// overlay and Close already handle it, and reporting here would dismiss it
    /// from under the person reading how long the call was.
    func testACallThatEndsOnItsOwnScreenIsNotReported() {
        let bar = CallBarHost()
        bar.observe(phase: .connected, now: Date())
        XCTAssertFalse(bar.observe(phase: .ended, now: Date()))
    }

    /// The next call starts on the screen, whatever the last one was put away as.
    func testANewCallDoesNotInheritACollapse() {
        let bar = CallBarHost()
        bar.observe(phase: .connected, now: Date())
        bar.collapse()
        bar.observe(phase: .ended, now: Date())
        bar.observe(phase: .ringing, now: Date())

        XCTAssertFalse(bar.collapsed, "a new call inherited the last one's collapse")
    }

    // MARK: - The one line

    /// The bar and the call screen print the same words for the same state. Two
    /// surfaces spelling one status line differently is the drift the shared rule
    /// exists to stop, and the strings are the one part of it that lives in
    /// Swift.
    func testTheBarPrintsTheSameWordsAsTheCallScreen() {
        XCTAssertEqual(
            CallBarPresentation.line(.calling),
            CallStatusText.line(phase: .calling, durationMs: nil))
        XCTAssertEqual(
            CallBarPresentation.line(.connecting),
            CallStatusText.line(phase: .connecting, durationMs: nil))
        XCTAssertEqual(
            CallBarPresentation.line(.reconnecting),
            CallStatusText.line(phase: .reconnecting(reason: "ice"), durationMs: nil))
        XCTAssertEqual(
            CallBarPresentation.line(.elapsed(ms: 724_000)),
            CallStatusText.line(phase: .connected, durationMs: 724_000))
        XCTAssertEqual(CallBarPresentation.line(.elapsed(ms: 724_000)), "12:04")
    }

    /// The sum type is what holds "the state or the running time, never two at
    /// once" (section 4). Asked for every phase, the line is exactly one of the
    /// four things — and a time only when there is a call to have been running.
    func testTheLineSaysTheStateOrTheTimeAndNeverBoth() {
        let states = ["Calling...", "Connecting...", "Reconnecting..."]
        for phase in everyPhase {
            let line = CallBarPresentation.line(
                callBarStatus(phase: phase, connectedForMs: 724_000))
            if phase == .connected {
                XCTAssertEqual(line, "12:04")
            } else {
                XCTAssertTrue(
                    states.contains(line), "\(phase) said \(line), which is neither")
            }
        }
    }

    /// A duration is read as a duration, not as "one two colon zero four"
    /// (section 8).
    func testTheSpokenLineReadsADurationAsADuration() {
        XCTAssertEqual(
            CallBarPresentation.spokenLine(.elapsed(ms: 724_000)), "12 minutes 4 seconds")
        XCTAssertEqual(CallBarPresentation.spokenLine(.connecting), "Connecting...")
    }

    /// The bar is one button, spoken as the whole sentence section 4.5 gives.
    func testTheWholeBarIsSpokenAsOneSentence() {
        XCTAssertEqual(
            CallBarPresentation.spokenLabel(
                name: "Anna", isPerson: true, status: .elapsed(ms: 724_000)),
            "Call with Anna, 12 minutes 4 seconds, return to call")
    }

    /// …and a call nobody is "the other person" on is not read "Call with Group
    /// call".
    func testAGroupCallIsNotSpokenAsACallWithACall() {
        XCTAssertEqual(
            CallBarPresentation.spokenLabel(
                name: "Group call", isPerson: false, status: .connecting),
            "Group call, Connecting..., return to call")
    }

    // MARK: - The label

    /// Section 4.5 asks for "the same label the call screen uses", so both read
    /// one function. The formula is the call screen's own, moved rather than
    /// copied.
    func testTheBarAndTheScreenNameTheCallTheSameWay() {
        XCTAssertEqual(
            CallPeerName.callLabel(profileName: "Anna", groupCall: false), "Anna")
        XCTAssertEqual(
            CallPeerName.callLabel(profileName: nil, groupCall: false),
            CallPeerName.placeholder,
            "a nameless 1x1 call must fall back to the app's one placeholder")
        XCTAssertEqual(
            CallPeerName.callLabel(profileName: nil, groupCall: true), "Group call")
        XCTAssertEqual(
            CallPeerName.callLabel(profileName: "  ", groupCall: true), "Group call",
            "whitespace is not a name")
    }

    // MARK: - The swipe

    /// The iOS way back down (section 4.5's table): a downward swipe on the call
    /// screen.
    func testADownwardSwipePutsTheCallAway() {
        XCTAssertTrue(CallBarPresentation.isCollapseSwipe(CGSize(width: 0, height: 120)))
        XCTAssertTrue(CallBarPresentation.isCollapseSwipe(CGSize(width: -20, height: 90)))
    }

    /// Everything that is not that swipe leaves the call screen alone. Ending a
    /// call is one tap away on this screen, so a drag that dismissed it by
    /// accident would put the person one tap from hanging up on someone.
    func testNothingElsePutsTheCallAway() {
        XCTAssertFalse(
            CallBarPresentation.isCollapseSwipe(CGSize(width: 0, height: 20)),
            "a short drag is not a swipe")
        XCTAssertFalse(
            CallBarPresentation.isCollapseSwipe(CGSize(width: 200, height: 70)),
            "a mostly sideways drag is not a downward swipe")
        XCTAssertFalse(
            CallBarPresentation.isCollapseSwipe(CGSize(width: 0, height: -120)),
            "an upward swipe is not a collapse")
        XCTAssertFalse(CallBarPresentation.isCollapseSwipe(.zero))
    }
}
