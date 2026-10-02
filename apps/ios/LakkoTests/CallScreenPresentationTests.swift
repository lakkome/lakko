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

    /// A one-to-one call, voice or video, on every screen size: Microphone,
    /// Camera, Settings, End.
    func testAOneToOneCallIsMicrophoneCameraSettingsEnd() {
        for layout in [CallScreenLayout.portrait, .video] {
            XCTAssertEqual(
                CallScreenPresentation.controls(layout: layout, canInvite: true, hasOthers: true),
                [.microphone, .camera, .settings, .end],
                "a 1x1 \(layout.rawValue) call has no list of people and no link to offer")
        }
    }

    /// The same row for a voice call and a video one, because the camera
    /// button is what turns a voice call into a video one — the row does not
    /// change underneath the user mid-call.
    func testAVideoCallShowsTheSameControlsAsAVoiceCall() {
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

    /// A group call: Microphone, Camera, Participants, Settings, End. There is
    /// no chat in a call and no menu of leftovers behind the row: the chat
    /// control sent nothing anywhere, and a control that does nothing is worse
    /// than none (removed 2026-10-02).
    func testAGroupCallRowIsMicrophoneCameraParticipantsSettingsEnd() {
        XCTAssertEqual(
            CallScreenPresentation.controls(layout: .gallery, canInvite: true, hasOthers: true),
            [.microphone, .camera, .participants, .settings, .end])
        XCTAssertEqual(
            CallScreenPresentation.controls(layout: .gallery, canInvite: true, hasOthers: false),
            [.microphone, .camera, .participants, .settings, .end],
            "a call that can hand out a link always has someone to invite")
        XCTAssertEqual(
            CallScreenPresentation.controls(layout: .gallery, canInvite: false, hasOthers: true),
            [.microphone, .camera, .participants, .settings, .end])
    }

    /// A call placed inside a conversation has no link. Alone in it,
    /// `Participants` would open a list of one, so the control is absent
    /// rather than present and useless.
    func testParticipantsIsAbsentWhenAloneInACallWithNoLink() {
        XCTAssertEqual(
            CallScreenPresentation.controls(layout: .gallery, canInvite: false, hasOthers: false),
            [.microphone, .camera, .settings, .end])
    }

    /// The whole set of controls a call can carry. There is no Speaker
    /// control on any call, and a chat, a raised hand, a screen share, a
    /// separate Invite or a More menu coming back would have to add a case
    /// here.
    func testNoControlExistsThatDoesNothing() {
        XCTAssertEqual(
            CallControl.allCases, [.microphone, .camera, .participants, .settings, .end])
    }

    // MARK: - What Participants opens (call-links section 3)

    /// On a link call the press opens the app's own menu; on a call inside a
    /// conversation it opens the list of people directly, because a menu with
    /// one item is a second step for nothing.
    func testWhatPressingParticipantsDoes() {
        XCTAssertEqual(
            CallScreenPresentation.participantsPress(canInvite: true, hasOthers: false), .menu)
        XCTAssertEqual(
            CallScreenPresentation.participantsPress(canInvite: true, hasOthers: true), .menu)
        XCTAssertEqual(
            CallScreenPresentation.participantsPress(canInvite: false, hasOthers: true),
            .participants)
        XCTAssertEqual(
            CallScreenPresentation.participantsPress(canInvite: false, hasOthers: false),
            .nothing)
    }

    /// Up to three items, in this order. `Participants` only when somebody
    /// else is in the call: alone, the menu is the two invite items.
    ///
    /// `Invite contacts` does not depend on having a conversation: the rule
    /// takes no such input, so an account with an empty chat list gets the
    /// item, and the list it opens says it is empty. Only a guest account
    /// does not get it.
    func testTheParticipantsMenu() {
        XCTAssertEqual(
            CallScreenPresentation.participantsMenu(hasOthers: true, guest: false),
            [.participants, .copyLink, .inviteContacts])
        XCTAssertEqual(
            CallScreenPresentation.participantsMenu(hasOthers: false, guest: false),
            [.copyLink, .inviteContacts])
        XCTAssertEqual(
            CallScreenPresentation.participantsMenu(hasOthers: false, guest: true),
            [.copyLink])
        XCTAssertEqual(
            CallScreenPresentation.participantsMenu(hasOthers: true, guest: true),
            [.participants, .copyLink])
    }

    /// The control says what the press opens: the menu of people and
    /// invitations, or the list (to show, or to hide).
    func testTheParticipantsControlNamesWhatItOpens() {
        XCTAssertEqual(CallControl.participants.label, "Participants")
        XCTAssertEqual(
            CallStatusText.spokenParticipantsLabel(press: .menu, listOpen: false),
            "Participants and invitations")
        XCTAssertEqual(
            CallStatusText.spokenParticipantsLabel(press: .participants, listOpen: false),
            "Show participants")
        XCTAssertEqual(
            CallStatusText.spokenParticipantsLabel(press: .participants, listOpen: true),
            "Hide participants")
    }

    /// End is last, wherever it appears, because it is the one button the hand
    /// must find without looking.
    func testEndIsAlwaysTheLastControl() {
        for layout in [CallScreenLayout.portrait, .video, .gallery] {
            XCTAssertEqual(CallScreenPresentation.controls(layout: layout).last, .end)
        }
    }

    /// Every kind of call keeps Microphone, Camera, Settings and End in one
    /// order, so the thumb learns one row rather than two.
    func testTheSharedControlsKeepOneOrderEverywhere() {
        for layout in [CallScreenLayout.portrait, .video, .gallery] {
            let row = CallScreenPresentation.controls(
                layout: layout, canInvite: true, hasOthers: true
            ).filter { $0 != .participants }
            XCTAssertEqual(row, [.microphone, .camera, .settings, .end])
        }
    }

    // MARK: - Fitting the row to the screen (section 4.1.1)

    /// The iPhone 17 that showed the defect: 402 pt across, the bar inset 12 pt
    /// each side. Five columns at the spec sizes come to 452 pt. (The fit
    /// itself; today's rows hold four controls, or five on a group call,
    /// which is sized by its own rule below.)
    func testFiveControlsFitA402PointPhoneWithTheEndButtonOnTheScreen() {
        let fit = CallScreenPresentation.fitControlBar(availableWidth: 402 - 24, count: 5)
        let row = 5 * fit.column + 5 * CallScreenPresentation.controlGap
            + 2 * CallScreenPresentation.barHorizontalPadding
        XCTAssertLessThanOrEqual(row, 378 + 0.01, "row \(row) must fit 378 pt")
        XCTAssertEqual(fit.rowWidth(count: 5), row, accuracy: 0.01)
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

    /// The controls that open something are not states, so they are always
    /// the quiet face.
    func testAControlThatOpensSomethingIsNeverTheLoudFace() {
        for control in [CallControl.participants, .settings] {
            XCTAssertEqual(CallScreenPresentation.face(control, on: true), .outline)
            XCTAssertEqual(CallScreenPresentation.face(control, on: false), .outline)
        }
    }

    // MARK: - What can be pressed (section 4's state table)

    /// While the call rings out there is nothing to mute yet. `Settings` is
    /// live from the first second: a person who cannot be heard wants it
    /// before anyone answers. `Participants` is not about the media and
    /// exists only on a group call, which rings nobody.
    func testOnlyEndAndSettingsAreActiveWhileTheCallRingsOut() {
        XCTAssertTrue(CallScreenPresentation.isEnabled(.settings, phase: .calling))
        XCTAssertFalse(
            CallScreenPresentation.isEnabled(.settings, phase: .ended(endedAt: Date())))
        for control in [CallControl.microphone, .camera, .end] {
            XCTAssertEqual(
                CallScreenPresentation.isEnabled(control, phase: .calling),
                control == .end,
                "\(control.rawValue) while calling")
        }
        XCTAssertTrue(CallScreenPresentation.isEnabled(.participants, phase: .calling))
        XCTAssertFalse(
            CallScreenPresentation.isEnabled(.participants, phase: .ended(endedAt: Date())))
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
        XCTAssertEqual(CallScreenPresentation.autoHideAfter, 5)
    }

    /// A group call's controls never hide: its tiles are not one picture the
    /// bar stands in front of, the tile area ends above the bar, and a tap on
    /// a tile pins it (`docs/ux/group-call-grid.md` section 6.2).
    func testAGroupCallNeverHidesItsControls() {
        XCTAssertFalse(CallScreenPresentation.autoHides(layout: .gallery, phase: .connected))
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
        XCTAssertEqual(CallControl.microphone.label, "Microphone")
        XCTAssertEqual(CallControl.settings.label, "Settings")
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

    /// Section 3.4: a group call rings nobody, and neither does a call joined
    /// from a link. While it starts its line reads `Connecting...`, never
    /// `Calling...`.
    func testAGroupCallNeverSaysCalling() {
        for phase in [CallUiState.CallPhase.preparing, .calling, .connecting] {
            XCTAssertEqual(
                CallStatusText.line(phase: phase, durationMs: nil, groupCall: true),
                "Connecting...", "\(phase)")
            XCTAssertEqual(
                CallStatusText.spokenLine(phase: phase, durationMs: nil, groupCall: true),
                "Connecting", "\(phase)")
        }
        XCTAssertEqual(
            CallStatusText.line(phase: .connected, durationMs: 724_000, groupCall: true), "12:04")
    }

    // MARK: - A group call's row fits one line (section 4.1)

    /// The row a phone shows: no words under the circles, and the circles
    /// sized from the width and the number of controls so the whole row is on
    /// the screen. A row that wrapped took half a phone screen (found live
    /// 2026-10-02).
    func testOnAPhoneTheGroupRowHasNoWordsAndFitsTheWidth() {
        for width in [CGFloat(320), 375, 393, 402, 430] {
            for count in 4...5 {
                let available = width - 24
                let fit = CallScreenPresentation.fitGroupControlBar(
                    availableWidth: available, screenSize: CGSize(width: width, height: 800),
                    count: count)
                XCTAssertFalse(fit.showsLabels, "width \(width)")
                XCTAssertGreaterThanOrEqual(
                    fit.diameter, CallScreenPresentation.compactGroupMinDiameter)
                XCTAssertLessThanOrEqual(
                    fit.diameter, CallScreenPresentation.compactGroupMaxDiameter)
                XCTAssertGreaterThan(fit.endDiameter, fit.diameter)
                XCTAssertLessThanOrEqual(
                    fit.rowWidth(count: count), available + 0.01,
                    "\(count) controls must fit \(available) pt")
            }
        }
    }

    /// The same numbers the web row computes: circles of
    /// `(width - 50 - count * 8) / count`, between 38 and 52. Six circles on a
    /// 320 pt screen come to 37 and stop at the floor; the five of a phone's
    /// group row reach the cap.
    func testTheCompactGroupRowAgreesWithTheWebRow() {
        let six = CallScreenPresentation.fitGroupControlBar(
            availableWidth: 375 - 24, screenSize: CGSize(width: 375, height: 667), count: 6)
        XCTAssertEqual(six.diameter, (375 - 50 - 6 * 8) / 6, accuracy: 0.01)
        let floor = CallScreenPresentation.fitGroupControlBar(
            availableWidth: 320 - 24, screenSize: CGSize(width: 320, height: 568), count: 6)
        XCTAssertEqual(floor.diameter, CallScreenPresentation.compactGroupMinDiameter)
        let fit = CallScreenPresentation.fitGroupControlBar(
            availableWidth: 402 - 24, screenSize: CGSize(width: 402, height: 874), count: 5)
        XCTAssertEqual(fit.diameter, CallScreenPresentation.compactGroupMaxDiameter)
        XCTAssertEqual(fit.endDiameter, fit.diameter + 6, accuracy: 0.01)
        XCTAssertEqual(fit.gap, 8)
        // The bar is short enough to leave the screen to the tiles.
        XCTAssertLessThan(fit.barHeight, 80)
    }

    /// A phone on its side is short, not narrow: the words go there too.
    func testAShortScreenDropsTheWordsToo() {
        let fit = CallScreenPresentation.fitGroupControlBar(
            availableWidth: 874 - 24, screenSize: CGSize(width: 874, height: 402), count: 5)
        XCTAssertFalse(fit.showsLabels)
        XCTAssertEqual(fit.diameter, CallScreenPresentation.compactGroupMaxDiameter)
    }

    /// With room, the group row keeps its words at slightly smaller circles
    /// (56 pt, End 64 pt).
    func testWithRoomTheGroupRowKeepsItsWords() {
        let fit = CallScreenPresentation.fitGroupControlBar(
            availableWidth: 1024 - 24, screenSize: CGSize(width: 1024, height: 768), count: 5)
        XCTAssertTrue(fit.showsLabels)
        XCTAssertEqual(fit.diameter, CallScreenPresentation.groupControlDiameter)
        XCTAssertEqual(fit.endDiameter, CallScreenPresentation.groupEndDiameter)
    }

    // MARK: - A group call's tiles (group-call-grid section 7)

    /// A connection is drawn only for fair or worse, never for a constant
    /// "good".
    func testATileShowsItsConnectionOnlyWhenItIsFairOrWorse() {
        XCTAssertFalse(CallScreenPresentation.connectionIsWorthShowing(.excellent))
        XCTAssertFalse(CallScreenPresentation.connectionIsWorthShowing(.good))
        XCTAssertTrue(CallScreenPresentation.connectionIsWorthShowing(.fair))
        XCTAssertTrue(CallScreenPresentation.connectionIsWorthShowing(.poor))
        XCTAssertTrue(CallScreenPresentation.connectionIsWorthShowing(.bad))
    }

    func testTheInitialsScaleWithTheTile() {
        let strip = CallScreenPresentation.tileInitialsDiameter(
            tileSize: CGSize(width: 104, height: 78))
        let grid = CallScreenPresentation.tileInitialsDiameter(
            tileSize: CGSize(width: 179, height: 238))
        let stage = CallScreenPresentation.tileInitialsDiameter(
            tileSize: CGSize(width: 366, height: 470))
        XCTAssertLessThan(strip, grid)
        XCTAssertLessThan(grid, stage)
        XCTAssertLessThan(strip, 78, "the initials fit a strip tile")
        XCTAssertLessThanOrEqual(stage, 96)
    }

    func testANarrowTileCarriesTheFirstNameOnly() {
        XCTAssertEqual(CallScreenPresentation.tileName("Anna Karenina", tileWidth: 104), "Anna")
        XCTAssertEqual(
            CallScreenPresentation.tileName("Anna Karenina", tileWidth: 180), "Anna Karenina")
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
        XCTAssertEqual(CallStatusText.spokenControlLabel(.settings, on: false), "Call settings")
        XCTAssertEqual(CallStatusText.spokenControlLabel(.end, on: false), "End call")
    }
}
