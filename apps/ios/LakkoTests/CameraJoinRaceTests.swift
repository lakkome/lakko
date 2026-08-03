// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// Tests for the camera-state join-race guard on the iOS SFU group-call path.
///
/// The race: in an SFU group call a newcomer B subscribing to an already
/// camera-muted peer A receives A's still-present (disabled) video track, so
/// `onRemoteVideoTrack` fires and would set A's roster `camOn = true` (a
/// frozen / black tile). A's `onRemoteMediaState(off)` re-announce also arrives,
/// but the two are NOT ordered — if the track-add lands AFTER the media-state,
/// A is wrongly shown camera-on.
///
/// The fix is a sticky, adapter-local last-known camera-state map that
/// `onRemoteVideoTrack` defers to: media-state is authoritative, so a track
/// arrival never flips a known-muted peer back on. The decision is the pure
/// `joinRaceCameraOn(trackPresent:lastKnown:)` on BOTH `SfuCallUi` and
/// `MultiJoinerCall`; both adapters build a real `SfuCallSession` in `init`
/// (needs a live socket / JWT), so — like `MediaStateRosterTests` — these tests
/// exercise the exact decision + the exact two-callback sequence the closures
/// run, applied through the real `CallUiState`.
@MainActor
final class CameraJoinRaceTests: XCTestCase {

    // MARK: - Pure decision (the real production function, both adapters)

    /// The atomic resolution contract, asserted against both adapters' real
    /// `joinRaceCameraOn` (they must stay identical).
    func testJoinRaceCameraResolution() {
        for resolve in [SfuCallUi.joinRaceCameraOn, MultiJoinerCall.joinRaceCameraOn] {
            // A track arrival for a peer we already know is MUTED stays off
            // (the race case — the disabled track must not re-show the tile).
            XCTAssertFalse(resolve(true, false), "known-muted peer stays off on track arrival")
            // A track arrival for an UNKNOWN peer (no prior media-state) defaults
            // on — they published video.
            XCTAssertTrue(resolve(true, nil), "unknown peer defaults on")
            // A track arrival for a peer we know is ON stays on.
            XCTAssertTrue(resolve(true, true), "known-on peer stays on")
            // A nil track (removal) is always off, whatever the last-known state.
            XCTAssertFalse(resolve(false, true))
            XCTAssertFalse(resolve(false, nil))
            XCTAssertFalse(resolve(false, false))
        }
    }

    // MARK: - Ordering (the two-callback interplay through a real CallUiState)

    /// The race case: `onRemoteMediaState(off)` THEN a track arrival keeps
    /// `camOn = false` — the newcomer subscribing to an already-muted peer.
    func testMediaStateOffThenTrackArrivalKeepsCameraOff() {
        for adapter in Adapter.allCases {
            let h = Harness(adapter: adapter)
            h.applyMediaState(false)   // A is muted (authoritative)
            h.applyTrack(present: true) // A's still-present disabled track arrives
            XCTAssertEqual(h.camOn, false, "\(adapter): a muted peer must stay off when its track arrives late")
        }
    }

    /// The reverse order: a track arrival THEN `onRemoteMediaState(off)` also
    /// ends `camOn = false` (the media-state clears the transiently-on tile).
    func testTrackArrivalThenMediaStateOffEndsCameraOff() {
        for adapter in Adapter.allCases {
            let h = Harness(adapter: adapter)
            h.applyTrack(present: true)  // no prior state -> transiently on
            XCTAssertEqual(h.camOn, true, "\(adapter): unknown peer shows on until told otherwise")
            h.applyMediaState(false)     // muted re-announce clears it
            XCTAssertEqual(h.camOn, false, "\(adapter): media-state off clears the tile")
        }
    }

    /// A track arrival with NO prior media-state shows the peer on (they
    /// published video).
    func testTrackArrivalWithNoPriorStateShowsCameraOn() {
        for adapter in Adapter.allCases {
            let h = Harness(adapter: adapter)
            h.applyTrack(present: true)
            XCTAssertEqual(h.camOn, true, "\(adapter): a fresh publisher shows on")
        }
    }

    /// Peer-left forgets the sticky state, so a rejoin under the same id starts
    /// clean: after a muted A leaves and rejoins, its first track arrival shows
    /// on again (the stale `false` must not persist).
    func testPeerLeftClearsStickyState() {
        for adapter in Adapter.allCases {
            let h = Harness(adapter: adapter)
            h.applyMediaState(false)     // A muted
            h.applyPeerLeft()            // A leaves -> sticky state forgotten
            h.applyTrack(present: true)  // A rejoins, publishes video
            XCTAssertEqual(h.camOn, true, "\(adapter): a rejoining peer is not held off by stale state")
        }
    }

    // MARK: - Harness mirroring the adapter's two-callback sequence

    private enum Adapter: CaseIterable, CustomStringConvertible {
        case sfuCallUi, multiJoinerCall
        var description: String { self == .sfuCallUi ? "SfuCallUi" : "MultiJoinerCall" }
        /// The real production decision function for this adapter.
        var resolve: (Bool, Bool?) -> Bool {
            self == .sfuCallUi ? SfuCallUi.joinRaceCameraOn : MultiJoinerCall.joinRaceCameraOn
        }
    }

    /// Replays the exact operations the adapter closures run for one remote peer
    /// `"peer-a"` against a real `CallUiState`:
    ///   * `onRemoteMediaState`  -> `lastKnown[id] = cameraOn; setParticipantCamera(id, cameraOn)`
    ///   * `onRemoteVideoTrack`  -> `setParticipantCamera(id, joinRaceCameraOn(present, lastKnown[id]))`
    ///   * `onPeerLeft`          -> `lastKnown.removeValue(id)` (+ roster removal, elided)
    /// The decision uses each adapter's REAL `joinRaceCameraOn`; the sticky map
    /// write is the trivial line the closures run verbatim.
    @MainActor
    private final class Harness {
        private let id = "peer-a"
        private let state = CallUiState()
        private let adapter: Adapter
        private var lastKnown: [String: Bool] = [:]

        init(adapter: Adapter) {
            self.adapter = adapter
            state.setParticipants([
                CallUiState.Participant(
                    id: id, displayName: id, isLocal: false,
                    micOn: true, camOn: false, handRaised: false,
                    quality: .good, speaking: false, screenSharing: false
                )
            ])
        }

        func applyMediaState(_ cameraOn: Bool) {
            lastKnown[id] = cameraOn
            state.setParticipantCamera(id, cameraOn)
        }

        func applyTrack(present: Bool) {
            let on = adapter.resolve(present, lastKnown[id])
            state.setParticipantCamera(id, on)
        }

        func applyPeerLeft() {
            lastKnown.removeValue(forKey: id)
        }

        var camOn: Bool? {
            state.participants.first(where: { $0.id == id })?.camOn
        }
    }
}
