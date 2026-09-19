// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// `docs/ux/calls.md` section 4.4 — which tile a group call draws large. The
/// same rule as the web `featuredParticipant` and the Android
/// `featuredParticipant`, pinned here so the three screens cannot drift back to
/// each picking someone else.
@MainActor
final class FeaturedParticipantTests: XCTestCase {

    private func tile(
        _ id: String, isLocal: Bool = false, camOn: Bool = false, speaking: Bool = false
    ) -> CallUiState.Participant {
        CallUiState.Participant(
            id: id, displayName: id, isLocal: isLocal,
            micOn: true, camOn: camOn, handRaised: false,
            quality: .good, speaking: speaking, screenSharing: false)
    }

    private var me: CallUiState.Participant { tile("me", isLocal: true, camOn: true) }

    /// The defect: the first remote held the big slot with an avatar in it
    /// while other cameras ran in small tiles. With no remote camera the honest
    /// layout is an equal grid, the local camera notwithstanding.
    func testNobodyIsFeaturedWhenNoOneElseIsSendingVideo() {
        let roster = [me, tile("a"), tile("b"), tile("c")]
        XCTAssertNil(CallScreenPresentation.featured(participants: roster, pinnedId: nil))
        XCTAssertEqual(
            CallScreenPresentation.gallery(participants: roster, featured: nil).map(\.id),
            ["me", "a", "b", "c"])
    }

    func testTheFirstRemoteWithACameraIsFeaturedInRosterOrder() {
        let roster = [me, tile("a"), tile("b", camOn: true), tile("c", camOn: true)]
        let featured = CallScreenPresentation.featured(participants: roster, pinnedId: nil)
        XCTAssertEqual(featured?.id, "b")
        XCTAssertEqual(
            CallScreenPresentation.gallery(participants: roster, featured: featured).map(\.id),
            ["me", "a", "c"])
    }

    func testASpeakingRemoteWithACameraOutranksAnEarlierSilentOne() {
        let roster = [me, tile("a", camOn: true), tile("b", camOn: true, speaking: true)]
        XCTAssertEqual(
            CallScreenPresentation.featured(participants: roster, pinnedId: nil)?.id, "b")
    }

    func testASpeakerWithoutACameraIsNeverFeatured() {
        XCTAssertEqual(
            CallScreenPresentation.featured(
                participants: [me, tile("a", speaking: true), tile("b", camOn: true)], pinnedId: nil
            )?.id,
            "b")
        XCTAssertNil(
            CallScreenPresentation.featured(
                participants: [me, tile("a", speaking: true)], pinnedId: nil))
    }

    func testTheLocalTileIsNeverFeaturedWhileAnyoneElseIsPresent() {
        XCTAssertNil(CallScreenPresentation.featured(participants: [me, tile("a")], pinnedId: nil))
    }

    func testTheLocalTileIsFeaturedWhenAlone() {
        XCTAssertEqual(CallScreenPresentation.featured(participants: [me], pinnedId: nil)?.id, "me")
        XCTAssertEqual(
            CallScreenPresentation.featured(
                participants: [tile("solo", isLocal: true)], pinnedId: nil
            )?.id,
            "solo")
    }

    func testAPinWinsOverTheCameraRule() {
        let roster = [me, tile("a"), tile("b", camOn: true)]
        XCTAssertEqual(
            CallScreenPresentation.featured(participants: roster, pinnedId: "a")?.id, "a")
    }

    func testAPinNamingNobodyInTheRosterIsIgnored() {
        let roster = [me, tile("a"), tile("b", camOn: true)]
        XCTAssertEqual(
            CallScreenPresentation.featured(participants: roster, pinnedId: "gone")?.id, "b")
    }

    func testTheSlotPassesToTheNextCameraWhenTheFeaturedOneStops() {
        let before = [me, tile("a", camOn: true), tile("b", camOn: true)]
        let after = [me, tile("a"), tile("b", camOn: true)]
        XCTAssertEqual(
            CallScreenPresentation.featured(participants: before, pinnedId: nil)?.id, "a")
        XCTAssertEqual(
            CallScreenPresentation.featured(participants: after, pinnedId: nil)?.id, "b")
    }

    func testTheStateDelegatesToTheRule() {
        let state = CallUiState()
        state.setParticipants([me, tile("a"), tile("b", camOn: true)])
        XCTAssertEqual(state.featuredParticipant?.id, "b")
        XCTAssertEqual(state.galleryParticipants.map(\.id), ["me", "a"])
        state.pin("a")
        XCTAssertEqual(state.featuredParticipant?.id, "a")
    }
}
