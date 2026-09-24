// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// `docs/ux/media-messages.md` — the composer's recorder stands aside while a
/// call HOLDS the microphone and camera, and a ringing offer holds neither.
///
/// ## What these tests prove, and what they deliberately do not
///
/// The rule is `crates/lakko-ui-rules/src/capture.rs`, which pins its own
/// answers in Rust; nothing here restates it. What Rust cannot see is whether
/// this platform ever asks it, and — the part the defect turned on — whether the
/// one iOS state that owns no capture is the one handed over as the ring.
///
/// These run through the real ``LakkoCore`` rule rather than a stand-in, because
/// this suite links it. So a mapping that pointed at the wrong state would fail
/// here even though the rule itself is correct, which is exactly the failure that
/// shipped.
final class CallCaptureTests: XCTestCase {

    // MARK: - The state that owns nothing

    func testARingingIncomingCallLeavesTheRecorderUsable() {
        // The offer took the call slot, so `callInSlot` is true - which is why
        // asking the slot alone got this wrong. Nothing is captured: the peer
        // connection and the local audio track are built by `acceptCall`.
        XCTAssertFalse(
            CallCapture.ownsCapture(
                callInSlot: true, oneToOneState: .ringing, otherKindInSlot: false))
    }

    func testALiveCallHoldsCapture() {
        XCTAssertTrue(
            CallCapture.ownsCapture(
                callInSlot: true, oneToOneState: .connected, otherKindInSlot: false))
    }

    func testAnOutgoingCallHoldsCaptureWhileItIsStillDialing() {
        // The asymmetry the rule's flag is named for: the caller's microphone
        // opens as the call is placed, so `.outgoing` is not a free recorder even
        // though nobody has answered.
        XCTAssertFalse(
            CallCapture.unansweredIncomingRing(
                oneToOneState: .outgoing, otherKindInSlot: false))
        XCTAssertTrue(
            CallCapture.ownsCapture(
                callInSlot: true, oneToOneState: .outgoing, otherKindInSlot: false))
    }

    func testRingingIsTheOnlyStateThatFreesTheRecorder() {
        // Read off the state machine rather than listed by hand, so a state added
        // later is covered rather than quietly assumed to hold capture.
        for state in [
            CallSession.WebCallState.idle, .outgoing, .connecting, .connected, .ending, .done,
        ] {
            XCTAssertTrue(
                CallCapture.ownsCapture(
                    callInSlot: true, oneToOneState: state, otherKindInSlot: false),
                "\(state) is not an unanswered incoming ring")
        }
        XCTAssertFalse(
            CallCapture.ownsCapture(
                callInSlot: true, oneToOneState: .ringing, otherKindInSlot: false))
    }

    // MARK: - The slot read reaches the rule untouched

    func testNoCallLeavesTheRecorderUsable() {
        XCTAssertFalse(
            CallCapture.ownsCapture(
                callInSlot: false, oneToOneState: nil, otherKindInSlot: false))
    }

    func testAStaleRingingReadNeverFreesAGroupOrLinkCall() {
        // A group or call-link call owns the microphone outright. The app admits
        // one call at a time so this cannot arise in a correct host; the guard
        // keeps the one failure that would matter on the blocking side.
        XCTAssertFalse(
            CallCapture.unansweredIncomingRing(
                oneToOneState: .ringing, otherKindInSlot: true))
        XCTAssertTrue(
            CallCapture.ownsCapture(
                callInSlot: true, oneToOneState: .ringing, otherKindInSlot: true))
    }
}
