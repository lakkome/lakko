// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// `docs/ux/calls.md` sections 4.5.1 and 4.5.4 — what the collapsed window and
/// the system window show, as values (``CallWindowContent``). The stage rule
/// and the picture rule are the shared ones of `lakko-ui-rules`, reached over
/// UniFFI, so these assert the window asks them the right question.
@MainActor
final class CallWindowContentTests: XCTestCase {

    private func person(
        _ id: String, _ name: String = "", local: Bool = false, cam: Bool = false,
        screen: Bool = false
    ) -> CallUiState.Participant {
        CallUiState.Participant(
            id: id, displayName: name, isLocal: local, micOn: true, camOn: cam,
            handRaised: false, quality: .good, speaking: false, screenSharing: false,
            isScreen: screen)
    }

    private func shown(
        _ people: [CallUiState.Participant],
        pinned: String? = nil,
        speech: StageSpeech = .none,
        sharer: String? = nil,
        group: Bool,
        camera: Bool = false
    ) -> CallWindowShown {
        CallWindowContent.shown(
            layoutPeople: people, pinnedId: pinned, speech: speech,
            screenSharerId: sharer, groupCall: group, ownCameraSending: camera)
    }

    // MARK: - Who is on the picture

    func testAOneToOneShowsTheOtherPersonAndNoCount() {
        let result = shown([person("local", local: true), person("remote", "Anna")], group: false)
        XCTAssertEqual(result.stage?.id, "remote")
        XCTAssertEqual(result.name, "Anna")
        XCTAssertEqual(result.othersBeyond, 0)
    }

    func testAPersonAloneSeesThemselvesWithNoInset() {
        let result = shown([person("local", local: true, cam: true)], group: true, camera: true)
        XCTAssertEqual(result.stage?.id, "local")
        XCTAssertEqual(result.name, CallPeerName.tileLabel(displayName: "", isLocal: true))
        XCTAssertFalse(result.showsOwnInset, "no inset when the picture already is you")
    }

    func testTheOwnInsetShowsOnlyWhileTheCameraSends() {
        let people = [person("local", local: true, cam: true), person("remote", "Anna")]
        XCTAssertTrue(shown(people, group: false, camera: true).showsOwnInset)
        XCTAssertFalse(shown(people, group: false, camera: false).showsOwnInset)
    }

    func testAGroupCallShowsTheSpeakerAndCountsEveryoneElse() {
        let people = [
            person("local", local: true), person("peer-a", "Anna"), person("peer-b", "Boris"),
            person("peer-c", "Chen"),
        ]
        let result = shown(
            people, speech: StageSpeech(activeSpeakerId: "peer-b", lastSpokeAtMs: [:]), group: true)
        XCTAssertEqual(result.stage?.id, "peer-b")
        XCTAssertEqual(result.name, "Boris")
        XCTAssertEqual(result.othersBeyond, 2, "everyone beyond the person shown and you")
    }

    func testAPinBeatsTheSpeaker() {
        let people = [person("local", local: true), person("peer-a", "Anna"), person("peer-b", "Boris")]
        let result = shown(
            people, pinned: "peer-a",
            speech: StageSpeech(activeSpeakerId: "peer-b", lastSpokeAtMs: [:]), group: true)
        XCTAssertEqual(result.stage?.id, "peer-a")
    }

    func testAScreenShareIsThePictureAndItsSharerIsNotCounted() {
        let people = [
            person("local", local: true), person("peer-a", "Anna"), person("peer-b", "Boris"),
            person("share-peer-a", "Anna", cam: true, screen: true),
        ]
        let result = shown(people, sharer: "peer-a", group: true)
        XCTAssertTrue(result.isScreenShare)
        XCTAssertEqual(result.othersBeyond, 1, "Boris; the sharer is the person shown")
    }

    // MARK: - The count

    func testTheCountIsPrintedOnlyWhenThereIsSomeoneToCount() {
        XCTAssertNil(CallWindowContent.othersLabel(0))
        XCTAssertEqual(CallWindowContent.othersLabel(16), "+16")
    }

    // MARK: - The picture asked for

    func testTheWindowAsksForThePictureItsWidthGets() {
        XCTAssertEqual(
            CallWindowContent.wantedQuality(people: 2, windowWidthPx: 424, hasLight: true), .main,
            "a call of two asks for the main picture whatever the size")
        XCTAssertEqual(
            CallWindowContent.wantedQuality(people: 5, windowWidthPx: 200, hasLight: true), .light)
        XCTAssertEqual(
            CallWindowContent.wantedQuality(people: 5, windowWidthPx: 424, hasLight: true), .middle)
        XCTAssertEqual(
            CallWindowContent.wantedQuality(people: 5, windowWidthPx: 1500, hasLight: true), .middle,
            "the window is never counted as the stage")
        XCTAssertEqual(
            CallWindowContent.wantedQuality(people: 5, windowWidthPx: 200, hasLight: false), .middle,
            "a person with no light picture is asked for the middle one")
    }

    // MARK: - When there is a window

    func testOnlyAPlacedOrLiveCallHasAWindow() {
        XCTAssertFalse(CallWindowContent.hasWindow(.ringing), "a call only ringing has none")
        XCTAssertFalse(CallWindowContent.hasWindow(.idle))
        XCTAssertFalse(CallWindowContent.hasWindow(.ended))
        XCTAssertTrue(CallWindowContent.hasWindow(.calling))
        XCTAssertTrue(CallWindowContent.hasWindow(.connecting))
        XCTAssertTrue(CallWindowContent.hasWindow(.connected))
        XCTAssertTrue(CallWindowContent.hasWindow(.reconnecting))
    }
}
