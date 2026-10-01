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
    func testAVoiceCallShowsSpeakerMuteCameraAndEnd() {
        XCTAssertEqual(
            CallScreenPresentation.controls(layout: .portrait),
            [.speaker, .microphone, .camera, .end])
    }

    /// The same four, because the camera button is what turns a voice call into
    /// a video one — the row does not change underneath the user mid-call.
    func testAVideoCallShowsTheSameFourControls() {
        XCTAssertEqual(
            CallScreenPresentation.controls(layout: .video),
            CallScreenPresentation.controls(layout: .portrait))
    }

    /// The capability this pins: a call placed with a voice can still show
    /// something a minute later. A row briefly built as Speaker / Mute / End
    /// took that away, and dropping a capability to keep a row short is a
    /// feature lost to tidiness.
    func testAVoiceCallCanStillTurnItsCameraOn() {
        XCTAssertTrue(
            CallScreenPresentation.controls(layout: .portrait).contains(.camera),
            "a voice call must keep the button that makes it a video call")
        XCTAssertTrue(
            CallScreenPresentation.isEnabled(.camera, phase: .connected),
            "and it must be pressable once the call is up")
    }

    /// The rule the old screen broke worst: switching camera or moving the
    /// sound to the speaker is a one-thumb action mid-sentence, and a menu makes
    /// it two taps and a reading task.
    func testA1x1CallHasNoOverflowMenuAtAll() {
        for layout in [CallScreenLayout.portrait, .video] {
            XCTAssertFalse(
                CallScreenPresentation.controls(layout: layout).contains(.overflow),
                "a 1x1 \(layout.rawValue) call must not hide controls behind a menu")
        }
    }

    /// A group call may put participants and chat behind one button — but mute,
    /// camera, speaker and end stay in the row.
    func testAGroupCallKeepsTheFourControlsInTheRowBesideItsOverflow() {
        let controls = CallScreenPresentation.controls(layout: .gallery)
        XCTAssertTrue(controls.contains(.overflow))
        for required in [CallControl.speaker, .microphone, .camera, .end] {
            XCTAssertTrue(controls.contains(required), "\(required.rawValue) stays in the row")
        }
    }

    /// End is last, wherever it appears, because it is the one button the hand
    /// must find without looking.
    func testEndIsAlwaysTheLastControl() {
        for layout in [CallScreenLayout.portrait, .video, .gallery] {
            XCTAssertEqual(CallScreenPresentation.controls(layout: layout).last, .end)
        }
    }

    /// Every kind of call offers the four, in one order, so the thumb learns one
    /// row rather than two.
    func testTheFourControlsKeepOneOrderEverywhere() {
        for layout in [CallScreenLayout.portrait, .video, .gallery] {
            let row = CallScreenPresentation.controls(layout: layout)
                .filter { $0 != .overflow }
            XCTAssertEqual(row, [.speaker, .microphone, .camera, .end])
        }
    }

    // MARK: - Fitting the row to the screen (section 4.1.1)

    /// The iPhone 17 that showed the defect: 402 pt across, the bar inset 12 pt
    /// each side. Five columns at the spec sizes come to 452 pt.
    func testFiveControlsFitA402PointPhoneWithTheEndButtonOnTheScreen() {
        let fit = CallScreenPresentation.fitControlBar(availableWidth: 402 - 24, count: 5)
        let row = 5 * fit.column + 5 * CallScreenPresentation.controlGap
            + 2 * CallScreenPresentation.barHorizontalPadding
        XCTAssertLessThanOrEqual(row, 378 + 0.01, "row \(row) must fit 378 pt")
        XCTAssertLessThanOrEqual(fit.endDiameter, fit.column)
        XCTAssertGreaterThanOrEqual(fit.diameter, CallScreenPresentation.minimumControlDiameter)
        XCTAssertGreaterThan(fit.endDiameter, fit.diameter)
    }

    func testWithRoomToSpareTheControlsKeepTheSpecSizes() {
        let fit = CallScreenPresentation.fitControlBar(availableWidth: 800, count: 5)
        XCTAssertEqual(fit, .spec)
    }

    func testANarrowPhoneNeverDropsAControlUnderTheMinimumTouchTarget() {
        let fit = CallScreenPresentation.fitControlBar(availableWidth: 320 - 24, count: 5)
        XCTAssertGreaterThanOrEqual(fit.diameter, CallScreenPresentation.minimumControlDiameter)
        XCTAssertGreaterThanOrEqual(fit.endDiameter, CallScreenPresentation.minimumControlDiameter)
    }

    func testTheFourControlRowOfAOneToOneCallFitsAtSpecSizesOnA402PointPhone() {
        let fit = CallScreenPresentation.fitControlBar(availableWidth: 402 - 24, count: 4)
        XCTAssertEqual(fit.diameter, CallScreenPresentation.controlDiameter)
        XCTAssertEqual(fit.endDiameter, CallScreenPresentation.endDiameter)
    }

    /// The same numbers Android's `fitCallControlBar` produces for its 411 dp
    /// phone, so the two rows shrink alike.
    func testTheFitAgreesWithAndroidOnItsOwnWidth() {
        let fit = CallScreenPresentation.fitControlBar(availableWidth: 411 - 24, count: 5)
        // (411 - 24 - 32 - 60) / 5 = 59; 64 * 59 / 72 = 52.44.
        XCTAssertEqual(fit.column, 59, accuracy: 0.01)
        XCTAssertEqual(fit.diameter, 52.4444, accuracy: 0.01)
        XCTAssertEqual(fit.endDiameter, 59, accuracy: 0.01)
    }

    // MARK: - Which screen is drawn (section 4)

    func testAVoiceCallIsAPortrait() {
        XCTAssertEqual(
            CallScreenPresentation.layout(remoteCount: 1, remotePicture: false), .portrait)
    }

    /// And so is a video call that has not received a frame yet — there is
    /// nothing to fill the screen with, and a black rectangle is not an answer.
    func testAVideoCallWithNoFrameYetIsStillAPortrait() {
        XCTAssertEqual(
            CallScreenPresentation.layout(remoteCount: 0, remotePicture: false), .portrait)
    }

    func testAVideoCallWithAFrameFillsTheScreen() {
        XCTAssertEqual(
            CallScreenPresentation.layout(remoteCount: 1, remotePicture: true), .video)
    }

    /// Section 4.4: a group call is the gallery from its first second, whoever
    /// has joined yet. A second person joining a group call turned the screen
    /// into the 1x1 video fill, with no names and no own tile, on a live call.
    func testAGroupCallIsAGalleryWhoeverHasJoined() {
        XCTAssertEqual(
            CallScreenPresentation.layout(groupCall: true, remoteCount: 0, remotePicture: false),
            .gallery)
        XCTAssertEqual(
            CallScreenPresentation.layout(groupCall: true, remoteCount: 1, remotePicture: true),
            .gallery)
        XCTAssertEqual(
            CallScreenPresentation.layout(groupCall: true, remoteCount: 1, remotePicture: false),
            .gallery)
    }

    func testMoreThanOneOtherPersonIsAGallery() {
        XCTAssertEqual(
            CallScreenPresentation.layout(remoteCount: 2, remotePicture: false), .gallery)
        XCTAssertEqual(
            CallScreenPresentation.layout(remoteCount: 4, remotePicture: true), .gallery)
    }

    // MARK: - A track handle is not a picture

    /// The defect a live Android-to-iOS call found. A peer turning its camera on
    /// mid-call adds an `m=video` line, libwebrtc hands over a track the instant
    /// the SDP is applied, and iOS took that as "there is a picture": it went to
    /// the full-screen video layout and showed a dark surface for over a minute.
    func testATrackThatHasNotDrawnAFrameIsNotAPicture() {
        XCTAssertFalse(
            CallScreenPresentation.hasRemotePicture(trackPresent: true, framesArrived: false),
            "a track handle arrives with the SDP and proves nothing about frames")
    }

    func testAFrameWithoutATrackIsNotAPictureEither() {
        XCTAssertFalse(
            CallScreenPresentation.hasRemotePicture(trackPresent: false, framesArrived: true))
        XCTAssertFalse(
            CallScreenPresentation.hasRemotePicture(trackPresent: false, framesArrived: false))
    }

    func testATrackThatHasDrawnAFrameIsAPicture() {
        XCTAssertTrue(
            CallScreenPresentation.hasRemotePicture(trackPresent: true, framesArrived: true))
    }

    /// The whole chain, which is what actually failed: an announced-but-silent
    /// camera must leave the portrait up.
    func testAnAnnouncedButSilentCameraKeepsThePortrait() {
        let picture = CallScreenPresentation.hasRemotePicture(
            trackPresent: true, framesArrived: false)
        XCTAssertEqual(
            CallScreenPresentation.layout(remoteCount: 1, remotePicture: picture), .portrait)
    }

    // MARK: - The controls can never be stranded

    /// The worse of the two live defects: the row faded over a dark screen and
    /// the tap that should have brought it back did not, so there was no way to
    /// mute, show a camera, or hang up. The rule that makes that unreachable is
    /// that a screen with nothing to look at never hides its controls — so an
    /// announced-but-silent camera, being a portrait, keeps them.
    func testAnAnnouncedButSilentCameraNeverHidesTheControls() {
        let picture = CallScreenPresentation.hasRemotePicture(
            trackPresent: true, framesArrived: false)
        let layout = CallScreenPresentation.layout(remoteCount: 1, remotePicture: picture)
        XCTAssertFalse(
            CallScreenPresentation.autoHides(layout: layout, phase: .connected),
            "a call with no picture must keep its End button on the screen")
    }

    /// Stated as the general rule, over every state a 1x1 call can be in: the
    /// controls only ever hide when there is a picture underneath them.
    func testControlsOnlyHideWhenThereIsAPictureUnderneath() {
        for trackPresent in [true, false] {
            for framesArrived in [true, false] {
                let picture = CallScreenPresentation.hasRemotePicture(
                    trackPresent: trackPresent, framesArrived: framesArrived)
                let layout = CallScreenPresentation.layout(
                    remoteCount: 1, remotePicture: picture)
                XCTAssertEqual(
                    CallScreenPresentation.autoHides(layout: layout, phase: .connected),
                    picture,
                    "track=\(trackPresent) frames=\(framesArrived)")
            }
        }
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
        XCTAssertNil(CallStatusText.line(phase: .preparing, durationMs: nil))
    }

    // MARK: - Spoken labels (section 8)

    func testADurationIsAnnouncedAsADurationNotAsDigits() {
        XCTAssertEqual(
            CallStatusText.spokenLine(phase: .connected, durationMs: 724_000).map(l10nPlain),
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
