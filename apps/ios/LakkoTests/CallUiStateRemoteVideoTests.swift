// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import WebRTC
@testable import Lakko

/// Tests for the N-way SFU remote-video track map on `CallUiState`
/// (`setRemoteVideoTrack(_:forParticipant:)` + `remoteVideoTracks` +
/// `reset()`). The map is what lets an SFU group call render each
/// remote participant's own stream on its own tile — keyed on the
/// roster id (`"peer-<streamId>"`, the SFU pinning the MSID stream-id
/// to the source peer id) — instead of the single-handle 1×1 case.
///
/// The SwiftUI render itself (`ParticipantTile.videoSurface`) is not
/// unit-testable headless (it needs a hosted view hierarchy + a live
/// Metal renderer), so these tests cover the state layer the view
/// reads. Tracks are real `RTCVideoTrack`s built from a local factory
/// — creating a track needs no networking / DTLS, only the media
/// factory the app already links.
@MainActor
final class CallUiStateRemoteVideoTests: XCTestCase {

    /// One factory for the whole suite — creating a video track is
    /// media-only (no peer connection, no SSL/DTLS), so a plain
    /// factory is sufficient.
    private static let factory = RTCPeerConnectionFactory()

    private func makeTrack(_ id: String) -> RTCVideoTrack {
        let source = Self.factory.videoSource()
        return Self.factory.videoTrack(with: source, trackId: id)
    }

    func testSetForParticipantAddsEntry() {
        let state = CallUiState()
        let track = makeTrack("a")

        state.setRemoteVideoTrack(track, forParticipant: "peer-a")

        XCTAssertEqual(state.remoteVideoTracks.count, 1)
        XCTAssertTrue(state.remoteVideoTracks["peer-a"] === track)
    }

    func testSetForMultipleParticipantsKeepsEachKeyed() {
        let state = CallUiState()
        let a = makeTrack("a")
        let b = makeTrack("b")

        state.setRemoteVideoTrack(a, forParticipant: "peer-a")
        state.setRemoteVideoTrack(b, forParticipant: "peer-b")

        XCTAssertEqual(state.remoteVideoTracks.count, 2)
        XCTAssertTrue(state.remoteVideoTracks["peer-a"] === a)
        XCTAssertTrue(state.remoteVideoTracks["peer-b"] === b)
    }

    func testSetForParticipantReplacesExistingEntry() {
        let state = CallUiState()
        let first = makeTrack("a1")
        let second = makeTrack("a2")

        state.setRemoteVideoTrack(first, forParticipant: "peer-a")
        state.setRemoteVideoTrack(second, forParticipant: "peer-a")

        XCTAssertEqual(state.remoteVideoTracks.count, 1)
        XCTAssertTrue(state.remoteVideoTracks["peer-a"] === second)
    }

    func testNilRemovesOnlyThatParticipant() {
        let state = CallUiState()
        let a = makeTrack("a")
        let b = makeTrack("b")
        state.setRemoteVideoTrack(a, forParticipant: "peer-a")
        state.setRemoteVideoTrack(b, forParticipant: "peer-b")

        // Peer-left / camera-off cleanup for one peer.
        state.setRemoteVideoTrack(nil, forParticipant: "peer-a")

        XCTAssertEqual(state.remoteVideoTracks.count, 1)
        XCTAssertNil(state.remoteVideoTracks["peer-a"])
        XCTAssertTrue(state.remoteVideoTracks["peer-b"] === b)
    }

    func testNilForUnknownParticipantIsANoOp() {
        let state = CallUiState()
        let a = makeTrack("a")
        state.setRemoteVideoTrack(a, forParticipant: "peer-a")

        state.setRemoteVideoTrack(nil, forParticipant: "peer-does-not-exist")

        XCTAssertEqual(state.remoteVideoTracks.count, 1)
        XCTAssertTrue(state.remoteVideoTracks["peer-a"] === a)
    }

    func testResetClearsAllEntries() {
        let state = CallUiState()
        state.setRemoteVideoTrack(makeTrack("a"), forParticipant: "peer-a")
        state.setRemoteVideoTrack(makeTrack("b"), forParticipant: "peer-b")

        state.reset()

        XCTAssertTrue(state.remoteVideoTracks.isEmpty)
    }

    /// The tile's video surface is gated on `participant.camOn`, and a
    /// remote SFU participant is created `camOn: false`, so the
    /// per-participant camera flip is what actually makes the tile
    /// paint when a track arrives. It must touch ONLY the matching id.
    func testSetParticipantCameraFlipsOnlyThatParticipant() {
        let state = CallUiState()
        state.setParticipants([
            CallUiState.Participant(
                id: "local", displayName: "You", isLocal: true,
                micOn: true, camOn: true, handRaised: false,
                quality: .good, speaking: false, screenSharing: false
            ),
            CallUiState.Participant(
                id: "peer-a", displayName: "A", isLocal: false,
                micOn: true, camOn: false, handRaised: false,
                quality: .good, speaking: false, screenSharing: false
            ),
            CallUiState.Participant(
                id: "peer-b", displayName: "B", isLocal: false,
                micOn: true, camOn: false, handRaised: false,
                quality: .good, speaking: false, screenSharing: false
            ),
        ])

        state.setParticipantCamera("peer-a", true)

        XCTAssertEqual(state.participants.first(where: { $0.id == "peer-a" })?.camOn, true)
        // The other remote + the local participant are untouched.
        XCTAssertEqual(state.participants.first(where: { $0.id == "peer-b" })?.camOn, false)
        XCTAssertEqual(state.participants.first(where: { $0.id == "local" })?.camOn, true)

        // A nil-track arrival flips it back off (video stops).
        state.setParticipantCamera("peer-a", false)
        XCTAssertEqual(state.participants.first(where: { $0.id == "peer-a" })?.camOn, false)
    }

    func testSetParticipantCameraForUnknownIdIsANoOp() {
        let state = CallUiState()
        state.setParticipants([
            CallUiState.Participant(
                id: "peer-a", displayName: "A", isLocal: false,
                micOn: true, camOn: false, handRaised: false,
                quality: .good, speaking: false, screenSharing: false
            ),
        ])

        // A track that raced the join — no matching participant yet.
        state.setParticipantCamera("peer-nope", true)

        XCTAssertEqual(state.participants.count, 1)
        XCTAssertEqual(state.participants.first?.camOn, false)
    }

    /// The keyed map and the single 1×1 handle are independent — the
    /// keyed setter must not touch `remoteVideoTrack`, and the
    /// single-handle setter must not touch the map. This is what keeps
    /// the 1×1 P2P fallback in `CallScreen` from being disturbed by the
    /// SFU path (and vice versa).
    func testKeyedAndSingleHandleAreIndependent() {
        let state = CallUiState()
        let single = makeTrack("single")
        let keyed = makeTrack("keyed")

        state.setRemoteVideoTrack(single)
        state.setRemoteVideoTrack(keyed, forParticipant: "peer-x")

        XCTAssertTrue(state.remoteVideoTrack === single)
        XCTAssertTrue(state.remoteVideoTracks["peer-x"] === keyed)
        XCTAssertEqual(state.remoteVideoTracks.count, 1)
    }
}
