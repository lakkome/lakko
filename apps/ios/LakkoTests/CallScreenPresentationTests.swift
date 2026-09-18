// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// `docs/ux/calls.md` sections 4, 4.1 and 4.1.1 — the call screen's shape as
/// values: which controls exist, which can be pressed, how each circle is
/// drawn, what the one status line says, and when the row hides itself.
///
/// The rules being pinned are the ones the redesign exists for, and each of
/// them was wrong in a way a person noticed before a test did: the speaker
/// button was missing entirely, the controls people reach for mid-sentence were
/// behind an ellipsis menu, the highlighted button was the one that was OFF, and
/// a floating badge repeated the word the status line already carried.
@MainActor
final class CallScreenPresentationTests: XCTestCase {

    // MARK: - Which controls exist (section 4.1)

    /// Speaker is the control people reach for most on a voice call, and it was
    /// not on the screen at all.
    func testAVoiceCallShowsSpeakerMuteAndEnd() {
        XCTAssertEqual(
            CallScreenPresentation.controls(layout: .portrait, videoCall: false),
            [.speaker, .microphone, .end])
    }

    func testAVideoCallAddsTheCameraButton() {
        XCTAssertEqual(
            CallScreenPresentation.controls(layout: .video, videoCall: true),
            [.speaker, .microphone, .camera, .end])
    }

    /// The rule the old screen broke worst: switching camera or moving the
    /// sound to the speaker is a one-thumb action mid-sentence, and a menu makes
    /// it two taps and a reading task.
    func testA1x1CallHasNoOverflowMenuAtAll() {
        for videoCall in [true, false] {
            for layout in [CallScreenLayout.portrait, .video] {
                XCTAssertFalse(
                    CallScreenPresentation.controls(layout: layout, videoCall: videoCall)
                        .contains(.overflow),
                    "a 1x1 \(layout.rawValue) call must not hide controls behind a menu")
            }
        }
    }

    /// A group call may put participants and chat behind one button — but mute,
    /// camera, speaker and end stay in the row.
    func testAGroupCallKeepsTheFourControlsInTheRowBesideItsOverflow() {
        let controls = CallScreenPresentation.controls(layout: .gallery, videoCall: true)
        XCTAssertTrue(controls.contains(.overflow))
        for required in [CallControl.speaker, .microphone, .camera, .end] {
            XCTAssertTrue(controls.contains(required), "\(required.rawValue) stays in the row")
        }
    }

    /// End is last, wherever it appears, because it is the one button the hand
    /// must find without looking.
    func testEndIsAlwaysTheLastControl() {
        for layout in [CallScreenLayout.portrait, .video, .gallery] {
            for videoCall in [true, false] {
                XCTAssertEqual(
                    CallScreenPresentation.controls(layout: layout, videoCall: videoCall).last,
                    .end)
            }
        }
    }

    // MARK: - Which screen is drawn (section 4)

    func testAVoiceCallIsAPortrait() {
        XCTAssertEqual(
            CallScreenPresentation.layout(remoteCount: 1, remoteVideo: false), .portrait)
    }

    /// And so is a video call that has not received a frame yet — there is
    /// nothing to fill the screen with, and a black rectangle is not an answer.
    func testAVideoCallWithNoFrameYetIsStillAPortrait() {
        XCTAssertEqual(
            CallScreenPresentation.layout(remoteCount: 0, remoteVideo: false), .portrait)
    }

    func testAVideoCallWithAFrameFillsTheScreen() {
        XCTAssertEqual(
            CallScreenPresentation.layout(remoteCount: 1, remoteVideo: true), .video)
    }

    func testMoreThanOneOtherPersonIsAGallery() {
        XCTAssertEqual(
            CallScreenPresentation.layout(remoteCount: 2, remoteVideo: false), .gallery)
        XCTAssertEqual(
            CallScreenPresentation.layout(remoteCount: 4, remoteVideo: true), .gallery)
    }

    // MARK: - Which state is loud (section 4.1.1)

    /// The correction the screen needed most: a muted microphone is the solid,
    /// high-contrast one. The old screen filled the OFF button with the accent
    /// colour, which reads as "this is on".
    func testAMutedMicrophoneIsTheLoudFace() {
        XCTAssertEqual(CallScreenPresentation.face(.microphone, on: false), .solid)
        XCTAssertEqual(CallScreenPresentation.face(.microphone, on: true), .outline)
    }

    func testAStoppedCameraIsTheLoudFace() {
        XCTAssertEqual(CallScreenPresentation.face(.camera, on: false), .solid)
        XCTAssertEqual(CallScreenPresentation.face(.camera, on: true), .outline)
    }

    /// The speaker is the one control whose attention-worthy state is ON: the
    /// earpiece is the resting state, and the room hearing the call is the thing
    /// to notice.
    func testTheSpeakerIsLoudWhenTheRoomCanHear() {
        XCTAssertEqual(CallScreenPresentation.face(.speaker, on: true), .solid)
        XCTAssertEqual(CallScreenPresentation.face(.speaker, on: false), .outline)
    }

    // MARK: - What can be pressed (section 4's state table)

    /// "Calling: only End is active." There is nothing to mute yet.
    func testOnlyEndIsActiveWhileTheCallRingsOut() {
        for control in CallControl.allCases {
            XCTAssertEqual(
                CallScreenPresentation.isEnabled(control, phase: .calling),
                control == .end,
                "\(control.rawValue) while calling")
        }
    }

    func testEveryControlIsActiveOnceTheCallIsUp() {
        for phase in [CallUiState.CallPhase.connecting, .connected, .reconnecting(reason: "ice")] {
            for control in CallControl.allCases {
                XCTAssertTrue(
                    CallScreenPresentation.isEnabled(control, phase: phase),
                    "\(control.rawValue) in \(phase)")
            }
        }
    }

    /// End stays pressable even on a call that is over, because a screen with no
    /// working button on it is a trap.
    func testEndStaysPressableOnAnEndedCall() {
        XCTAssertTrue(
            CallScreenPresentation.isEnabled(.end, phase: .ended(endedAt: Date())))
        XCTAssertFalse(
            CallScreenPresentation.isEnabled(.microphone, phase: .ended(endedAt: Date())))
    }

    // MARK: - When the row hides (section 4)

    func testAConnectedVideoCallFadesItsControls() {
        XCTAssertTrue(CallScreenPresentation.autoHides(layout: .video, phase: .connected))
        XCTAssertTrue(CallScreenPresentation.autoHides(layout: .gallery, phase: .connected))
        XCTAssertEqual(CallScreenPresentation.autoHideAfter, 5)
    }

    /// A voice call's controls never hide: there is nothing to look at
    /// underneath, so hiding them only makes the user tap (section 4.2).
    func testAVoiceCallNeverHidesItsControls() {
        XCTAssertFalse(CallScreenPresentation.autoHides(layout: .portrait, phase: .connected))
    }

    /// Nor does a call that is not connected yet — the controls are what the
    /// caller is waiting with.
    func testControlsDoNotHideBeforeTheCallConnects() {
        for phase in [CallUiState.CallPhase.calling, .connecting, .reconnecting(reason: "ice")] {
            XCTAssertFalse(CallScreenPresentation.autoHides(layout: .video, phase: phase))
        }
    }

    // MARK: - Measurements (section 4.1.1)

    func testTheCirclesAreEqualAndEndIsBiggerAndSetApart() {
        XCTAssertEqual(CallScreenPresentation.controlDiameter, 64)
        XCTAssertEqual(CallScreenPresentation.endDiameter, 72)
        XCTAssertEqual(CallScreenPresentation.endGap, CallScreenPresentation.controlGap * 2)
        XCTAssertEqual(CallScreenPresentation.disabledOpacity, 0.4)
        XCTAssertGreaterThanOrEqual(CallScreenPresentation.minimumTouchTarget, 44)
    }

    /// The label names the action, never the state, so it does not change while
    /// the state does.
    func testTheLabelsNameTheAction() {
        XCTAssertEqual(CallControl.speaker.label, "Speaker")
        XCTAssertEqual(CallControl.microphone.label, "Mute")
        XCTAssertEqual(CallControl.camera.label, "Camera")
        XCTAssertEqual(CallControl.end.label, "End")
    }

    // MARK: - The one status line (section 4)

    func testTheStatusLineSaysOneThingPerState() {
        XCTAssertEqual(CallStatusText.line(phase: .calling, durationMs: nil), "Calling...")
        XCTAssertEqual(CallStatusText.line(phase: .connecting, durationMs: nil), "Connecting...")
        XCTAssertEqual(
            CallStatusText.line(phase: .reconnecting(reason: "ice"), durationMs: nil),
            "Reconnecting...")
        XCTAssertEqual(
            CallStatusText.line(phase: .ended(endedAt: Date()), durationMs: nil), "Call ended")
    }

    /// Connected, the line IS the clock — not the word "Connected" plus a clock
    /// somewhere else.
    func testAConnectedCallShowsTheRunningTimeAndNothingElse() {
        XCTAssertEqual(
            CallStatusText.line(phase: .connected, durationMs: 724_000), "12:04")
    }

    /// The reason for a separate phase: `Calling...` and `Connecting...` are
    /// different facts, and collapsing them made a ringing call claim to be
    /// connecting to someone who had not picked up.
    func testCallingAndConnectingAreNotTheSameLine() {
        XCTAssertNotEqual(
            CallStatusText.line(phase: .calling, durationMs: nil),
            CallStatusText.line(phase: .connecting, durationMs: nil))
    }

    /// The camera check has its own words; it is not a call yet.
    func testTheCameraCheckHasNoStatusLine() {
        XCTAssertNil(CallStatusText.line(phase: .cameraCheck, durationMs: nil))
    }

    // MARK: - Spoken labels (section 8)

    func testADurationIsAnnouncedAsADurationNotAsDigits() {
        XCTAssertEqual(
            CallStatusText.spokenLine(phase: .connected, durationMs: 724_000),
            "12 minutes 4 seconds")
    }

    func testEveryControlNamesTheActionAndTheResultingState() {
        XCTAssertEqual(
            CallStatusText.spokenControlLabel(.microphone, on: true), "Mute microphone")
        XCTAssertEqual(
            CallStatusText.spokenControlLabel(.microphone, on: false), "Unmute microphone")
        XCTAssertEqual(CallStatusText.spokenControlLabel(.camera, on: true), "Stop camera")
        XCTAssertEqual(CallStatusText.spokenControlLabel(.camera, on: false), "Start camera")
        XCTAssertEqual(CallStatusText.spokenControlLabel(.speaker, on: false), "Turn on speaker")
        XCTAssertEqual(CallStatusText.spokenControlLabel(.end, on: false), "End call")
    }
}
