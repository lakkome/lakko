// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import WebRTC

/// Chat-UI adapter for the iOS native `SfuCallSession` — the analog of
/// the web `createSfuCallUi` (`apps/web/src/calls/sfu-call-ui.ts`).
///
/// Binds the session's callbacks onto a `CallUiState` so the existing
/// `CallScreen` renders a live SFU roster with no per-call-type
/// branching in the view:
///   * lifecycle `State` -> `CallPhase` (+ the E2EE pill flips active on
///     `connected`, since the SFrame seed is armed at connect),
///   * `welcome` / `peer-joined` / `peer-left` -> the roster,
///   * a remote video track -> the sole-remote tile (per-tile N-way
///     video is a follow-up; audio plays out automatically).
///
/// The caller owns deriving the inputs (the JWT via `CallJwtClient`, the
/// seed via `Group.exportSecret("lakko/call/v1", callId, 32)`, the leaf
/// via `group.ownLeafIndex`, the SFU base URL from config) and passes
/// them in the `SfuCallSession.Config`; this adapter is pure glue.
///
/// The closures capture the `CallUiState` + the name resolver directly
/// (not `self`), so there is no init-order chicken-and-egg and no retain
/// cycle through the session.
@MainActor
final class SfuCallUi {
    /// The underlying session (for direct control if needed).
    let session: SfuCallSession
    private let ui: CallUiState

    init(
        config: SfuCallSession.Config,
        ui: CallUiState,
        localDisplayName: String = "You",
        resolveDisplayName: @escaping (_ peerId: String, _ accountPseudonym: String, _ peerTag: String) -> String = { _, pseudonym, _ in
            pseudonym.isEmpty ? "Peer" : String(pseudonym.prefix(8))
        }
    ) {
        self.ui = ui
        let audio = config.audio
        let video = config.video
        // Canonical call safety number derived from the shared seed — the
        // same string every participant derives, shown in the E2EE pill
        // for an out-of-band man-in-the-middle check. Display-only +
        // non-critical: a derivation failure (only on a wrong-length seed,
        // which the MLS exporter never emits) must never break the call,
        // so it degrades to no fingerprint.
        let safetyNumber = try? callSafetyNumber(seed: config.seed)

        let callbacks = SfuCallSession.Callbacks(
            onStateChange: { state in
                switch state {
                case .connecting:
                    ui.setPhase(.connecting)
                case .connected:
                    ui.setPhase(.connected)
                    // SFrame is armed at connect, so media is E2E
                    // encrypted the moment we are connected. The safety
                    // number (derived from the same shared seed) lets
                    // participants verify the E2EE root key out-of-band.
                    ui.setE2ee(CallUiState.E2eeState(active: true, shortFingerprint: safetyNumber))
                case .failed, .closed:
                    ui.setPhase(.ended(endedAt: Date()))
                case .idle:
                    break
                }
            },
            onWelcome: { selfPeerId, roster in
                let local = Self.localParticipant(localDisplayName, audio: audio, video: video)
                let remotes = roster
                    .filter { $0.peerId != selfPeerId }
                    .map {
                        Self.remoteParticipant(
                            resolveDisplayName($0.peerId, $0.accountPseudonym, $0.peerTag),
                            peerId: $0.peerId
                        )
                    }
                ui.setParticipants([local] + remotes)
            },
            onPeerJoined: { peerId, accountPseudonym, peerTag in
                ui.participantJoined(
                    Self.remoteParticipant(
                        resolveDisplayName(peerId, accountPseudonym, peerTag),
                        peerId: peerId
                    )
                )
            },
            onPeerLeft: { peerId in
                ui.participantLeft(Self.participantId(forPeer: peerId))
            },
            onRemoteVideoTrack: { track, _ in
                // Sole-remote video (the 2-party case). Per-tile N-way
                // video on iOS needs a per-participant track map — a
                // follow-up, like the web side.
                ui.setRemoteVideoTrack(track)
            },
            onLocalVideoTrack: { track in
                // Local camera (video call) -> self-preview tile.
                ui.setLocalVideoTrack(track)
            },
            onServerError: { _, _ in }
        )
        self.session = SfuCallSession(config: config, callbacks: callbacks)
    }

    /// Open the SFU connection + publish (delegates to the session).
    func connect() async throws {
        try await session.connect()
    }

    /// Tear down (the session drives the UI to `.ended` via its
    /// state callback). Idempotent.
    func hangUp() {
        session.hangUp()
    }

    /// Mute / unmute the local mic — drives the real track + mirrors the
    /// store's local-participant flag.
    func setMicrophoneEnabled(_ on: Bool) {
        session.setMicrophoneEnabled(on)
        ui.setMic(on)
    }

    /// Enable / disable the local camera — drives the real track + mirrors
    /// the store's local-participant flag. Only meaningful for a video
    /// call (the video track is added at connect when `config.video`).
    func setCameraEnabled(_ on: Bool) {
        session.setCameraEnabled(on)
        ui.setCamera(on)
    }

    // MARK: - Roster helpers

    /// Stable roster id for a remote peer.
    private static func participantId(forPeer peerId: String) -> String {
        "peer-\(peerId)"
    }

    private static func localParticipant(
        _ name: String,
        audio: Bool,
        video: Bool
    ) -> CallUiState.Participant {
        CallUiState.Participant(
            id: "local",
            displayName: name,
            isLocal: true,
            micOn: audio,
            camOn: video,
            handRaised: false,
            quality: .good,
            speaking: false,
            screenSharing: false
        )
    }

    private static func remoteParticipant(
        _ name: String,
        peerId: String
    ) -> CallUiState.Participant {
        CallUiState.Participant(
            id: participantId(forPeer: peerId),
            displayName: name,
            isLocal: false,
            micOn: true,
            camOn: false,
            handRaised: false,
            quality: .good,
            speaking: false,
            screenSharing: false
        )
    }
}
