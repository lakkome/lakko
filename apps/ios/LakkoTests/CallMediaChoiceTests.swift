// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// `docs/ux/calls.md` section 3 — the kind of call the caller chose is the kind
/// that is placed, and the kind the callee is told.
///
/// The defect these pin: the phone button opened the pre-join with the camera
/// already on, Join offered `video: true`, and the callee's banner read "Video
/// call" for a call the caller had asked to make with their voice.
final class CallMediaChoiceTests: XCTestCase {

    // MARK: - What the pre-join opens with

    func testThePhoneButtonOpensTheJoinScreenWithTheCameraOff() {
        XCTAssertFalse(CallMediaChoice.prejoinCameraOn(for: .voice))
    }

    func testTheVideoButtonOpensTheJoinScreenWithTheCameraOn() {
        XCTAssertTrue(CallMediaChoice.prejoinCameraOn(for: .video))
    }

    func testTheToolbarsVideoFlagPicksTheButton() {
        XCTAssertEqual(CallMediaChoice.button(video: false), .voice)
        XCTAssertEqual(CallMediaChoice.button(video: true), .video)
    }

    // MARK: - What is actually offered

    /// The whole point of the pre-join: the toggles, not the button, decide.
    func testTheOfferCarriesTheToggles() {
        let media = CallMediaChoice.offeredMedia(micOn: true, cameraOn: false)
        XCTAssertTrue(media.audio)
        XCTAssertFalse(media.video)
    }

    /// A voice call whose camera the user turned on before joining IS a video
    /// call, and is announced as one — the button does not overrule the user.
    func testAVoiceCallWhoseCameraTheUserTurnedOnIsOfferedAsVideo() {
        let opened = CallMediaChoice.prejoinCameraOn(for: .voice)
        XCTAssertFalse(opened)
        let media = CallMediaChoice.offeredMedia(micOn: true, cameraOn: true)
        XCTAssertTrue(media.video)
        XCTAssertEqual(CallMediaChoice.kindLabel(video: media.video), "Video call")
    }

    /// And the reverse: a video call joined with the camera turned off is a
    /// voice call on the wire and on the callee's screen.
    func testAVideoCallWhoseCameraTheUserTurnedOffIsOfferedAsVoice() {
        let media = CallMediaChoice.offeredMedia(micOn: true, cameraOn: false)
        XCTAssertFalse(media.video)
        XCTAssertEqual(CallMediaChoice.kindLabel(video: media.video), "Voice call")
    }

    /// A muted join still carries the camera choice — the two toggles are
    /// independent.
    func testAMutedMicrophoneDoesNotTurnTheCameraOff() {
        let media = CallMediaChoice.offeredMedia(micOn: false, cameraOn: true)
        XCTAssertFalse(media.audio)
        XCTAssertTrue(media.video)
    }

    // MARK: - End to end, button to callee

    /// The chain the defect broke, in one assertion per button: what the
    /// caller tapped, what the pre-join shows, what the offer carries, and the
    /// word the callee reads.
    func testTheWordTheCalleeReadsMatchesTheButtonWhenNobodyTouchesTheToggles() {
        for (button, expected) in [(CallButtonKind.voice, "Voice call"), (.video, "Video call")] {
            let camera = CallMediaChoice.prejoinCameraOn(for: button)
            let media = CallMediaChoice.offeredMedia(micOn: true, cameraOn: camera)
            XCTAssertEqual(
                CallMediaChoice.kindLabel(video: media.video), expected,
                "the \(button.rawValue) button must announce \(expected)")
        }
    }
}
