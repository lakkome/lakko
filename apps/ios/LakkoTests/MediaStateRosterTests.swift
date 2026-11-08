// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// Tests for the camera-off-cleanup receive path on the iOS SFU group-call
/// path: a remote peer's `composeMediaState` signal, carried over the opaque
/// SFU `signal-relay`, is classified via `dispatchIncomingEnvelope` and mapped
/// onto that participant's roster `camOn` gate.
///
/// A remote camera OFF is a track-`isEnabled = false` MUTE that fires NO track
/// event, so `onRemoteVideoTrack` never fires and the tile would otherwise keep
/// painting a frozen last frame. The media-state signal is the only thing that
/// deterministically flips `camOn` — so this covers BOTH camera-off and
/// camera-back-on.
///
/// `SfuCallSession.dispatchSignalRelay` is private and needs a live socket, so
/// these tests exercise the exact sequence it runs — the same FFI calls in the
/// same order (`sfuEncodePayload(composeMediaState(...))` on the send side ->
/// `sfuDecodePayload` -> `dispatchIncomingEnvelope` classify on the receive
/// side) — and then apply the decoded flag through `setParticipantCamera` the
/// way `SfuCallUi.onRemoteMediaState` does.
@MainActor
final class MediaStateRosterTests: XCTestCase {

    /// The stable roster tile id for a remote peer — the same
    /// `"peer-<peerId>"` shape `SfuCallUi` keys tiles on.
    private func participantId(forPeer peerId: String) -> String {
        "peer-\(peerId)"
    }

    private func remote(_ id: String, camOn: Bool) -> CallUiState.Participant {
        CallUiState.Participant(
            id: id, displayName: id, isLocal: false,
            micOn: true, camOn: camOn, handRaised: false,
            quality: .good, speaking: false, screenSharing: false
        )
    }

    /// The full send -> relay -> classify contract: `composeMediaState` bytes
    /// survive the base64 relay round-trip and classify as `.mediaState` with
    /// the sender's flag intact, for both on and off.
    func testMediaStateRoundTripsAndClassifies() throws {
        for cameraOn in [true, false] {
            for microphoneOn in [true, false] {
                // Send side (mirrors `broadcastMediaState` -> `sendSignalRelayToCall`,
                // whose compose helper base64s the bytes into the relay payload).
                let payloadB64 = sfuEncodePayload(
                    payload: composeMediaState(cameraOn: cameraOn, microphoneOn: microphoneOn))
                // Receive side (mirrors `dispatchSignalRelay`).
                let payload = try sfuDecodePayload(payloadB64: payloadB64)
                let env = try dispatchIncomingEnvelope(plaintext: payload)
                XCTAssertEqual(env.kind, .mediaState)
                XCTAssertEqual(env.cameraOn, cameraOn)
                XCTAssertEqual(env.microphoneOn, microphoneOn)
            }
        }
    }

    /// The end-to-end receive path: a decoded media-state signal flips ONLY the
    /// sending peer's tile — camera OFF hides it, camera-back-ON re-shows it —
    /// and never touches another peer or the local participant.
    func testRemoteMediaStateFlipsOnlyThatPeersCamera() throws {
        let state = CallUiState()
        state.setParticipants([
            CallUiState.Participant(
                id: "local", displayName: "You", isLocal: true,
                micOn: true, camOn: true, handRaised: false,
                quality: .good, speaking: false, screenSharing: false
            ),
            // "a" starts camera-on (a track already arrived); "b" stays off.
            remote(participantId(forPeer: "a"), camOn: true),
            remote(participantId(forPeer: "b"), camOn: false),
        ])

        // Peer "a" turns its camera OFF (a mute — no track event fires).
        try applyMediaState(cameraOn: false, from: "a", to: state)
        XCTAssertEqual(camOn(state, "peer-a"), false, "camera-off must hide the tile")
        // Untouched.
        XCTAssertEqual(camOn(state, "peer-b"), false)
        XCTAssertEqual(camOn(state, "local"), true)

        // Peer "a" turns its camera BACK ON (a track re-enable — again no event).
        try applyMediaState(cameraOn: true, from: "a", to: state)
        XCTAssertEqual(camOn(state, "peer-a"), true, "camera-back-on must re-show the tile")
        XCTAssertEqual(camOn(state, "peer-b"), false)
    }

    /// A signal for a peer not yet in the roster (the relay raced the join) is a
    /// no-op — `setParticipantCamera` patches by id and skips an absent one.
    func testUnknownPeerSignalIsANoOp() throws {
        let state = CallUiState()
        state.setParticipants([remote(participantId(forPeer: "a"), camOn: false)])

        try applyMediaState(cameraOn: true, from: "ghost", to: state)

        XCTAssertEqual(state.participants.count, 1)
        XCTAssertEqual(camOn(state, "peer-a"), false)
    }

    // MARK: - Helpers mirroring the production receive path

    /// Run the exact `dispatchSignalRelay` -> `onRemoteMediaState` sequence: a
    /// peer `from` broadcasts `composeMediaState(cameraOn:)`, and the receiver
    /// classifies + applies it to the roster.
    private func applyMediaState(cameraOn: Bool, from: String, to state: CallUiState) throws {
        let payloadB64 = sfuEncodePayload(
            payload: composeMediaState(cameraOn: cameraOn, microphoneOn: true))
        let payload = try sfuDecodePayload(payloadB64: payloadB64)
        let env = try dispatchIncomingEnvelope(plaintext: payload)
        guard env.kind == .mediaState, let flag = env.cameraOn else {
            return XCTFail("expected a media-state envelope")
        }
        state.setParticipantCamera(participantId(forPeer: from), flag)
    }

    private func camOn(_ state: CallUiState, _ id: String) -> Bool? {
        state.participants.first(where: { $0.id == id })?.camOn
    }
}
