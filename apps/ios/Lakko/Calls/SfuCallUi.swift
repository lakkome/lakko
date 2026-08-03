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
///   * a remote video track -> its own per-participant tile, keyed on
///     the MSID stream-id (== the source peer id, so the roster id is
///     `"peer-<streamId>"`); audio plays out automatically.
///
/// The caller owns deriving the inputs (the JWT via `CallJwtClient`, the
/// seed via `Group.exportSecret("lakko/call/v1", callId, 32)`, the leaf
/// via `group.ownLeafIndex`, the SFU base URL from config) and passes
/// them in the `SfuCallSession.Config`; this adapter is pure glue.
///
/// The closures capture the `CallUiState` + the name resolver directly
/// (not `self`), so there is no init-order chicken-and-egg and no retain
/// cycle through the session.
/// Relay so the session's `onVideoPublishState` callback (built at
/// construction, before `self` is fully initialised) can forward into a
/// mutable observer the caller sets later — mirrors the pattern
/// `MultiJoinerCall` uses to capture a value (not `self`) in its callbacks.
private final class VideoPublishStateRelay {
    var handler: ((SfuCallSession.VideoPublishState) -> Void)?
}

@MainActor
final class SfuCallUi {
    /// The underlying session (for direct control if needed).
    let session: SfuCallSession
    private let ui: CallUiState
    private let videoStateRelay: VideoPublishStateRelay

    /// Optional observer for the mid-call camera-on lifecycle (piece B) —
    /// the analog of the web `onVideoPublishState`. Set it to drive a
    /// camera-button spinner. The fail-safe revert itself is driven by
    /// ``publishVideo()``'s throw (the ChatViewModel catches it), not by this.
    var onVideoPublishState: ((SfuCallSession.VideoPublishState) -> Void)? {
        get { videoStateRelay.handler }
        set { videoStateRelay.handler = newValue }
    }

    init(
        config: SfuCallSession.Config,
        ui: CallUiState,
        localDisplayName: String = "You",
        resolveDisplayName: @escaping (_ peerId: String, _ accountPseudonym: String, _ peerTag: String) -> String = { _, pseudonym, _ in
            pseudonym.isEmpty ? "Peer" : String(pseudonym.prefix(8))
        }
    ) {
        self.ui = ui
        let relay = VideoPublishStateRelay()
        self.videoStateRelay = relay
        let audio = config.audio
        let video = config.video
        // Canonical call safety number derived from the shared seed — the
        // same string every participant derives, shown in the E2EE pill
        // for an out-of-band man-in-the-middle check. Display-only +
        // non-critical: a derivation failure (only on a wrong-length seed,
        // which the MLS exporter never emits) must never break the call,
        // so it degrades to no fingerprint.
        let safetyNumber = config.seed.flatMap { try? callSafetyNumber(seed: $0) }

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
                let id = Self.participantId(forPeer: peerId)
                // Drop the departed peer's video track first so its tile
                // stops rendering a stale last frame before the roster
                // removes the tile.
                ui.setRemoteVideoTrack(nil, forParticipant: id)
                ui.participantLeft(id)
            },
            onRemoteVideoTrack: { track, streamId in
                // N-way SFU remote video — the SFU pins the downstream
                // MSID stream-id to the source peer id, and the roster
                // participant id is `"peer-<peerId>"`, so route the track
                // to that participant's tile.
                //
                // A nil stream-id (a camera-off `didRemove rtpReceiver`
                // fires `onRemoteVideoTrack(nil, nil)` with no MSID) can't
                // target one map entry — a known follow-up that needs a
                // receiver->streamId map in `SfuCallSession`. We drop it
                // here rather than mis-clear a tile; the frozen last frame
                // clears when that peer leaves (peer-left cleanup above).
                guard let streamId else { return }
                let id = "peer-\(streamId)"
                ui.setRemoteVideoTrack(track, forParticipant: id)
                // Flip the tile's `camOn` so its video surface (gated on
                // `participant.camOn` in `CallScreen`) actually paints on
                // track arrival — and stops on a nil track. A remote
                // participant is created `camOn: false`, so without this
                // the per-peer track map populates but the tile never
                // renders. Mirrors the web `setParticipantCamera`.
                ui.setParticipantCamera(id, track != nil)
            },
            onLocalVideoTrack: { track in
                // Local camera (video call or a mid-call publishVideo) ->
                // self-preview tile.
                ui.setLocalVideoTrack(track)
            },
            onVideoPublishState: { state in
                // B2a — drive the one UI surface `CallScreen` reads (the
                // camera-button spinner + the self-tile "Starting camera…"
                // hint), so the SFU path matches the 1×1 `driveFrom` path.
                ui.setVideoPublishState(Self.mapVideoPublishState(state))
                // Forward into the caller-set observer (captured `relay`, not
                // `self`, so this is safe to build before `session` is assigned).
                relay.handler?(state)
            },
            onServerError: { _, _ in },
            onRemoteMediaState: { from, cameraOn in
                // Camera-off-cleanup: a remote peer broadcast its camera on/off
                // state. Flip that participant's roster `camOn` gate (keyed on
                // `"peer-<peerId>"`, the same tile id `onRemoteVideoTrack` uses).
                // This is the ONLY signal that clears a remote camera-OFF mute
                // (which fires no track event) — and re-shows the tile on
                // camera-back-on. UI-only; SFrame/seed/MLS untouched.
                ui.setParticipantCamera(Self.participantId(forPeer: from), cameraOn)
            }
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

    /// Rotate the call key on a mid-call MLS epoch change (G-18,
    /// ADR-0029). The caller re-derives the per-call seed at the new
    /// epoch and passes it; every attached SFrame transform retains its
    /// previous key for the overlap window so in-flight frames still
    /// open, then drops it. No-op if the seed is unchanged. Mirrors the
    /// web `SfuCallUiHandle.rekey`.
    func rekey(newSeed: Data) {
        session.rekey(newSeed: newSeed)
    }

    /// Mute / unmute the local mic — drives the real track + mirrors the
    /// store's local-participant flag.
    func setMicrophoneEnabled(_ on: Bool) {
        session.setMicrophoneEnabled(on)
        ui.setMic(on)
    }

    /// Enable / disable the local camera — drives the real track + mirrors
    /// the store's local-participant flag. Meaningful once a video sender
    /// exists (a video call, or after a mid-call ``publishVideo()``).
    func setCameraEnabled(_ on: Bool) {
        session.setCameraEnabled(on)
        ui.setCamera(on)
    }

    /// Turn the camera ON mid-call (piece B) — delegates to the session, which
    /// acquires the camera, SFrame-seals the new video sender, and
    /// renegotiates the first time (a call that started audio-only), or just
    /// re-enables an existing sender after. Idempotent + in-flight-guarded.
    /// Throws on a capture / attach failure so the caller can revert the
    /// toggle (fail-safe; the call stays up audio-only — invariant #4).
    func publishVideo() async throws {
        try await session.publishVideo()
    }

    // MARK: - Roster helpers

    /// B2a — map the SFU session's camera-on lifecycle onto the shared UI
    /// enum. Identity mapping (same cases); the explicit switch catches a
    /// future divergence at compile time.
    private static func mapVideoPublishState(
        _ s: SfuCallSession.VideoPublishState
    ) -> CallUiState.VideoPublishState {
        switch s {
        case .off: return .off
        case .starting: return .starting
        case .live: return .live
        case .failed: return .failed
        }
    }

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
