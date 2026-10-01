// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// `docs/ux/calls.md` section 3 — what each call button opens, and the rule that
/// the kind of call the caller placed is the kind the callee is told.
///
/// Two defects are pinned here. The old one: the phone button opened a pre-join
/// with the camera already live, so the callee's banner read "Video call" for a
/// call made with a voice. The new one: that pre-join existed at all — a card
/// asking the user to check a microphone, in front of a phone call.
final class CallMediaChoiceTests: XCTestCase {

    // MARK: - What each button opens (sections 3.1 and 3.2)

    /// Section 3.1 — no preparation screen, no question, no empty camera box.
    func testThePhoneButtonPlacesTheCallImmediately() {
        XCTAssertEqual(
            CallMediaChoice.startSurface(for: .voice, hasCamera: true),
            .dial(video: false))
    }

    /// And a phone with no camera makes no difference to a voice call.
    func testThePhoneButtonDialsOnADeviceWithNoCamera() {
        XCTAssertEqual(
            CallMediaChoice.startSurface(for: .voice, hasCamera: false),
            .dial(video: false))
    }

    /// Section 3.2 since 2026-09-21 — the video button dials at once, with the
    /// camera. It opened a camera check until then; the self-view that check
    /// existed for is on the calling screen, so nothing is lost but the press.
    func testTheVideoButtonDialsCarryingTheCamera() {
        XCTAssertEqual(
            CallMediaChoice.startSurface(for: .video, hasCamera: true),
            .dial(video: true))
    }

    /// Section 3.2 — a video call a device cannot make is placed with voice.
    func testTheVideoButtonPlacesAVoiceCallWhenThereIsNoCamera() {
        XCTAssertEqual(
            CallMediaChoice.startSurface(for: .video, hasCamera: false),
            .dial(video: false))
    }

    /// ...and says so, in one line.
    func testTheNoCameraDowngradeSaysSoInOneLine() {
        let notice = CallMediaChoice.noCameraNotice(for: .video, hasCamera: false)
        XCTAssertEqual(notice, "No camera available, so this is a voice call.")
        XCTAssertFalse(notice?.contains("\n") ?? true, "the notice is one line")
    }

    /// Nothing was downgraded, so nothing is said. A line that appears on every
    /// call is a line nobody reads.
    func testNothingIsSaidWhenNothingWasDowngraded() {
        XCTAssertNil(CallMediaChoice.noCameraNotice(for: .video, hasCamera: true))
        XCTAssertNil(CallMediaChoice.noCameraNotice(for: .voice, hasCamera: true))
        XCTAssertNil(CallMediaChoice.noCameraNotice(for: .voice, hasCamera: false))
    }

    func testTheToolbarsVideoFlagPicksTheButton() {
        XCTAssertEqual(CallMediaChoice.button(video: false), .voice)
        XCTAssertEqual(CallMediaChoice.button(video: true), .video)
    }

    // MARK: - What is actually offered

    /// The whole point of the camera check: its two round buttons, not the
    /// header button, decide what goes on the wire.
    func testTheOfferCarriesWhatTheButtonsWereLeftAt() {
        let media = CallMediaChoice.offeredMedia(micOn: true, cameraOn: false)
        XCTAssertTrue(media.audio)
        XCTAssertFalse(media.video)
    }

    /// A video call whose camera the user turned off on the check is a voice
    /// call on the wire and on the callee's screen.
    func testAVideoCallWhoseCameraTheUserTurnedOffIsOfferedAsVoice() {
        let media = CallMediaChoice.offeredMedia(micOn: true, cameraOn: false)
        XCTAssertFalse(media.video)
        XCTAssertEqual(CallMediaChoice.kindLabel(video: media.video), "Voice call")
    }

    /// A muted check still carries the camera choice — the two buttons are
    /// independent.
    func testAMutedMicrophoneDoesNotTurnTheCameraOff() {
        let media = CallMediaChoice.offeredMedia(micOn: false, cameraOn: true)
        XCTAssertFalse(media.audio)
        XCTAssertTrue(media.video)
    }

    // MARK: - End to end, button to callee

    /// The chain, per button: what the caller tapped, what it opened, what the
    /// offer carries when nobody touches anything, and the word the callee
    /// reads.
    func testTheWordTheCalleeReadsMatchesTheButton() {
        let cases: [(CallButtonKind, String)] = [(.voice, "Voice call"), (.video, "Video call")]
        for (button, expected) in cases {
            let camera: Bool
            switch CallMediaChoice.startSurface(for: button, hasCamera: true) {
            case .dial(let video):
                camera = video
            }
            let media = CallMediaChoice.offeredMedia(micOn: true, cameraOn: camera)
            XCTAssertEqual(
                CallMediaChoice.kindLabel(video: media.video), expected,
                "the \(button.rawValue) button must announce \(expected)")
        }
    }
}
