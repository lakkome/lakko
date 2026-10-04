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

/// Sticky last-known remote camera state, keyed by the roster tile id
/// (`"peer-<peerId>"`), so a video-track ARRIVAL for a peer we already
/// know is camera-muted is never wrongly flipped back on. This closes the
/// join-time race: a newcomer subscribing to an already-muted peer A gets
/// A's still-present (disabled) video track — `onRemoteVideoTrack` fires —
/// AND A's `onRemoteMediaState(off)` re-announce, in a non-deterministic
/// order; without the sticky map a late track-add would wrongly show A as
/// camera-on (a frozen/black tile). Media-state is authoritative; a track
/// arrival only defers to it.
///
/// A reference type (not `self`) so the init-time callbacks can capture it
/// before `session` is assigned, mirroring `VideoPublishStateRelay`. All
/// access stays on the `@MainActor` the existing `setParticipantCamera`
/// calls already run on — no new concurrency.
private final class CameraStateTracker {
    var lastKnownCameraOn: [String: Bool] = [:]
}

/// Per-participant remote-video probes, and which participants have actually
/// produced a picture.
///
/// ## Why the SFU path needed this at all
///
/// `CallScreen` chooses its layout with
/// `CallScreenPresentation.layout(remoteCount:remotePicture:)`: more than one
/// remote is a gallery, otherwise it is the full-screen video if there is a
/// picture and the portrait if there is not. `remotePicture` reads
/// `CallUiState.remoteVideoFlowing`, and until now that property had exactly
/// ONE writer in the whole app — the 1x1 `CallSession.driveFrom` path. The SFU
/// path never wrote it and never attached a `RemoteVideoFrameProbe`, so on a
/// group call it was permanently `false`.
///
/// With exactly one other person on a group call — the commonest group call
/// there is, two people — that made the layout permanently `.portrait`, which
/// draws no tiles at all. The other participant could be publishing camera,
/// the track could be arriving, SFrame could be opening every frame, and the
/// screen would still show a generic avatar and a timer. A live test read that
/// screen as "group video does not work"; what it showed was a screen
/// structurally incapable of showing video for a two-person call.
///
/// Three or more remotes go to the gallery and were unaffected, which is why
/// this survived.
///
/// A reference type (not `self`) so the init-time callbacks can capture it,
/// mirroring `CameraStateTracker`. Main-actor confined like the rest of them.
private final class RemotePictureTracker {
    /// Probe per participant id, held so it can be detached when that
    /// participant's track is replaced or the participant leaves.
    var probes: [String: RemoteVideoFrameProbe] = [:]
    /// The track each probe is attached to — a probe must be removed from the
    /// exact track it was added to.
    var probed: [String: RTCVideoTrack] = [:]
    /// Participants whose track has rendered at least one frame.
    var flowing: Set<String> = []
}

@MainActor
final class SfuCallUi {
    /// The underlying session (for direct control if needed).
    let session: SfuCallSession
    private let ui: CallUiState
    private let videoStateRelay: VideoPublishStateRelay
    /// Who is speaking, read on this device from the call's own audio
    /// (`docs/ux/group-call-grid.md` section 7.3).
    private let speaking: GroupSpeakingMonitor

    /// Optional observer for the mid-call camera-on lifecycle (piece B) —
    /// the analog of the web `onVideoPublishState`. Set it to drive a
    /// camera-button spinner. The fail-safe revert itself is driven by
    /// ``publishVideo()``'s throw (``GroupCallHost`` catches it), not by this.
    var onVideoPublishState: ((SfuCallSession.VideoPublishState) -> Void)? {
        get { videoStateRelay.handler }
        set { videoStateRelay.handler = newValue }
    }

    init(
        config: SfuCallSession.Config,
        ui: CallUiState,
        localDisplayName: String = "",
        // Never the SFU pseudonym (a piece of the account id). An empty name is
        // the tile's unnamed state, for the moment before the person's profile
        // reaches this device (``CallTileName``).
        resolveDisplayName: @escaping (_ peerId: String, _ accountPseudonym: String, _ peerTag: String) -> String = { _, _, _ in "" }
    ) {
        self.ui = ui
        let relay = VideoPublishStateRelay()
        self.videoStateRelay = relay
        let speaking = GroupSpeakingMonitor(ui: ui)
        self.speaking = speaking
        // Sticky last-known remote camera state (join-race guard, below).
        // Captured by the media-state / track / peer-left closures, not `self`.
        let cameraTracker = CameraStateTracker()
        // Per-participant video probes — what makes a two-person group call
        // able to reach the video layout at all (see `RemotePictureTracker`).
        let pictureTracker = RemotePictureTracker()
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
                    speaking.start()
                case .failed, .closed:
                    speaking.stop()
                    ui.clearRemoteLightVideoTracks()
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
                NSLog("SfuCallUi: roster from welcome remotes=%d", remotes.count)
                // The roster decides whether there is a SOLE remote, which is
                // what `remoteVideoFlowing` is about — so a track that arrived
                // before the roster did is only now resolvable.
                Self.refreshSolePicture(ui: ui, tracker: pictureTracker)
            },
            onPeerJoined: { peerId, accountPseudonym, peerTag in
                ui.participantJoined(
                    Self.remoteParticipant(
                        resolveDisplayName(peerId, accountPseudonym, peerTag),
                        peerId: peerId
                    )
                )
                // A second remote turns a sole-remote call into a gallery.
                Self.refreshSolePicture(ui: ui, tracker: pictureTracker)
            },
            onPeerLeft: { peerId in
                let id = Self.participantId(forPeer: peerId)
                // Forget the departed peer's sticky camera state so a later
                // rejoin under the same id starts from a clean slate.
                cameraTracker.lastKnownCameraOn.removeValue(forKey: id)
                Self.detachProbe(id: id, tracker: pictureTracker)
                // Drop the departed peer's video track first so its tile
                // stops rendering a stale last frame before the roster
                // removes the tile.
                ui.setRemoteVideoTrack(nil, forParticipant: id)
                ui.participantLeft(id)
                Self.refreshSolePicture(ui: ui, tracker: pictureTracker)
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
                //
                // Join-race guard: a track ARRIVAL for a peer we already
                // know is camera-muted (via a prior `onRemoteMediaState`)
                // must NOT flip them back on — the disabled track is still
                // present on the SFU and its add can land after the muted
                // re-announce. Defer to the sticky last-known state; an
                // unknown peer defaults on (they published video); a nil
                // track is always off.
                let on = Self.joinRaceCameraOn(
                    trackPresent: track != nil,
                    lastKnown: cameraTracker.lastKnownCameraOn[id]
                )
                ui.setParticipantCamera(id, on)
                // A track handle is not a picture — it arrives with the SDP,
                // and the gap to the first openable frame is codec negotiation,
                // the first keyframe and SFrame decryption. Probe for the frame
                // itself, exactly as the 1x1 path does, and re-probe on every
                // track change: a peer that stops and restarts its camera
                // produces a NEW track, and a stale `true` would send the
                // screen to the video layout before anything has been drawn.
                Self.detachProbe(id: id, tracker: pictureTracker)
                pictureTracker.flowing.remove(id)
                if let track {
                    NSLog(
                        "SfuCallUi: remote video track attached peer=%@; waiting for the first frame",
                        id)
                    let probe = RemoteVideoFrameProbe {
                        // Only the track this probe was attached to may flip
                        // the flag: a late frame from a replaced track must not
                        // claim the new one is flowing.
                        guard pictureTracker.probed[id] === track else { return }
                        NSLog("SfuCallUi: remote video first frame rendered peer=%@", id)
                        pictureTracker.flowing.insert(id)
                        Self.refreshSolePicture(ui: ui, tracker: pictureTracker)
                    }
                    pictureTracker.probes[id] = probe
                    pictureTracker.probed[id] = track
                    track.add(probe)
                }
                Self.refreshSolePicture(ui: ui, tracker: pictureTracker)
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
            onServerError: { code, detail in
                // Was `{ _, _ in }` — an SFU error frame (a refused SDP, a bad
                // relay target) reached the app and was discarded, so a call
                // the server had already objected to looked, on this screen,
                // like a call that was merely quiet.
                NSLog("SfuCallUi: server error code=%@ detail=%@", "\(code)", detail)
            },
            onRemoteMediaState: { from, cameraOn in
                // Camera-off-cleanup: a remote peer broadcast its camera on/off
                // state. Flip that participant's roster `camOn` gate (keyed on
                // `"peer-<peerId>"`, the same tile id `onRemoteVideoTrack` uses).
                // This is the ONLY signal that clears a remote camera-OFF mute
                // (which fires no track event) — and re-shows the tile on
                // camera-back-on. UI-only; SFrame/seed/MLS untouched.
                let id = Self.participantId(forPeer: from)
                // Media-state is authoritative — record it so a later (racy)
                // track arrival for a muted peer defers to it (join-race guard).
                cameraTracker.lastKnownCameraOn[id] = cameraOn
                ui.setParticipantCamera(id, cameraOn)
            },
            onUndecryptableReceiver: { streamId, _ in
                // One peer's stream will never decode. Mark that peer so its
                // tile says "Can't decrypt" rather than sitting blank, which is
                // what a bad uplink looks like. The call stays up: the other
                // participants are unaffected, and their media is fine.
                //
                // An absent stream id cannot be attributed to a tile. The SFU
                // pins the MSID to the source peer id on every forwarded track
                // (`services/sfu/src/media.rs`), so this is not expected; when
                // it happens the NSLog in the session is the whole record,
                // because inventing an attribution would put the badge on an
                // innocent peer.
                guard let streamId else { return }
                ui.markUndecryptable(Self.participantId(forPeer: streamId))
            },
            onRemoteLightVideoTrack: { track, peerId in
                // The light companion of that peer's camera (``LightStream``):
                // kept beside the main track for the peer's tile to draw, and
                // nothing else. No participant, no camera flag, no probe: it
                // is not what decides that there is a picture.
                ui.setRemoteLightVideoTrack(track, forParticipant: Self.participantId(forPeer: peerId))
            }
        )
        let session = SfuCallSession(config: config, callbacks: callbacks)
        self.session = session
        speaking.source = { [weak session] in await session?.sampleAudioLevels() }
    }

    /// Open the SFU connection + publish (delegates to the session).
    func connect() async throws {
        try await session.connect()
    }

    /// Tear down (the session drives the UI to `.ended` via its
    /// state callback). Idempotent.
    func hangUp() {
        speaking.stop()
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
    /// The roster participant id for an SFU peer. Internal (not private) so
    /// ``GroupCallNameResolver`` can re-label the same tile later without a
    /// second copy of the convention.
    static func participantId(forPeer peerId: String) -> String {
        "peer-\(peerId)"
    }

    // MARK: - Remote picture

    /// Detach the probe attached to `id`'s current track, if any. A probe must
    /// be removed from the exact track it was added to, so the track is held
    /// alongside it rather than looked up again.
    private static func detachProbe(id: String, tracker: RemotePictureTracker) {
        if let probe = tracker.probes[id], let track = tracker.probed[id] {
            track.remove(probe)
        }
        tracker.probes.removeValue(forKey: id)
        tracker.probed.removeValue(forKey: id)
    }

    /// Republish `CallUiState.remoteVideoFlowing` for the sole-remote case.
    ///
    /// That flag is what turns a one-other-person call into the full-screen
    /// video layout, and it only means anything when there IS exactly one other
    /// person: with two or more remotes the screen is a gallery whatever this
    /// says, and with nobody there is nothing to show. So it is derived from
    /// the roster every time either the roster or a picture changes, rather
    /// than latched — a peer leaving a three-way call turns the survivor into a
    /// sole remote whose picture was already flowing, and nothing else would
    /// notice.
    private static func refreshSolePicture(ui: CallUiState, tracker: RemotePictureTracker) {
        ui.setRemoteVideoFlowing(
            solePictureFlowing(
                soleRemoteId: ui.soleRemoteParticipantId, flowing: tracker.flowing))
    }

    /// The rule `refreshSolePicture` applies, separated so it can be tested
    /// without a live session: there is a sole-remote picture only when there
    /// is exactly one other person (the caller resolves that) AND that person's
    /// track has rendered a frame. No sole remote is no picture — never a
    /// latched leftover from when there was one.
    static func solePictureFlowing(soleRemoteId: String?, flowing: Set<String>) -> Bool {
        guard let soleRemoteId else { return false }
        return flowing.contains(soleRemoteId)
    }

    /// Join-race camera resolution (see `CameraStateTracker`): media-state is
    /// authoritative, so a video-track ARRIVAL defers to the last-known state —
    /// a known-muted peer stays off, an unknown peer defaults on (they
    /// published video), and a nil track (removal) is always off. Pure so it is
    /// unit-testable without a live `SfuCallSession`.
    static func joinRaceCameraOn(trackPresent: Bool, lastKnown: Bool?) -> Bool {
        trackPresent ? (lastKnown ?? true) : false
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
