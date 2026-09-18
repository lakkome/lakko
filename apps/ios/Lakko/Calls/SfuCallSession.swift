// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import WebRTC

/// Multi-party SFU-backed call session on iOS — the native analog of
/// the web `apps/web/src/calls/sfu-call-session.ts`.
///
/// Where the 1×1 `CallSession` connects two devices peer-to-peer over
/// DTLS-SRTP, this connects the device to the Rust SFU and runs SFrame
/// end-to-end media encryption on top (the SFU only ever sees SFrame
/// ciphertext — security invariant #2).
///
/// Composes the pieces built earlier in the arc:
///   * `SfuSignalingClient` — the `lakko-sfu-v1` WebSocket (typed
///     frames through `lakko_sfu_proto`).
///   * The call-admission JWT (minted by `CallJwtClient`).
///   * `WebRTCPeerConnection` — the libwebrtc pod, with its
///     `armSFrameEncryptors` / `armSFrameDecryptor` hooks that install
///     the native SFrame bridge on each sender / receiver.
///
/// ## Signaling flow (one `RTCPeerConnection` to the SFU)
///   * **Publish** (client-initiated, once at connect): capture the
///     local mic, arm SFrame on the sender(s), `createOffer`, send it
///     as `ClientFrame::SdpOffer`; the SFU replies `SdpAnswer`.
///   * **Subscribe** (SFU-initiated, ongoing): when another participant
///     publishes, the SFU dispatches an `SdpOffer`; the session answers
///     (`createAnswer` -> `sendSdpAnswer`), and the new remote track
///     surfaces through the pod's `onRemoteReceiver` (SFrame decryptor
///     armed) + `onRemoteVideoTrack` (UI). Server offers are processed
///     through a serialized queue so two applies never overlap.
///
/// The SFU's own ICE candidates ride inside the SDP, so this session
/// only trickles its LOCAL candidates up via `sendIceCandidate` (the
/// wire protocol has no server -> client ICE frame).
///
/// Media: when `config.video` is set the session captures the local
/// camera (`LocalCameraCapture`), adds the video track BEFORE arming
/// SFrame + building the offer (so the offer advertises a video m-line
/// and the video sender is SFrame-sealed alongside audio), and surfaces
/// the local track through `onLocalVideoTrack` for the self-preview tile.
/// Audio-only calls skip all of that. `connect()` refuses before any
/// capture if the SFrame native shim is unavailable (invariant #4).
@MainActor
final class SfuCallSession {

    enum State { case idle, connecting, connected, failed, closed }

    enum SfuCallError: Error, LocalizedError {
        case alreadyConnected
        case sframeUnavailable
        /// Mid-call camera-on (piece B): no usable camera on this device.
        case noCamera

        var errorDescription: String? {
            switch self {
            case .alreadyConnected:
                return "The call is already connected."
            case .sframeUnavailable:
                return "Media encryption is unavailable on this build."
            case .noCamera:
                return "No camera is available on this device."
            }
        }
    }

    /// Lifecycle of a mid-call camera-on (ADR-0032 arc, piece B) — the native
    /// analog of the web `VideoPublishState`. A group call started audio-only
    /// has no video sender; turning the camera on ACQUIRES the camera and
    /// renegotiates (adds a track after the initial offer):
    ///   * `off`      — no video published (the default; also after a failure).
    ///   * `starting` — camera capture + the addTrack/renegotiation are in flight.
    ///   * `live`     — the local video track is captured, SFrame-sealed, and
    ///                  added (self-preview renders; the SDP renegotiation runs
    ///                  in the background).
    ///   * `failed`   — permission denied / no camera / capture error; the caller
    ///                  reverts the toggle + surfaces a message. The call stays up
    ///                  audio-only (invariant #4 — no half-added / unsealed sender
    ///                  is ever offered to the SFU).
    enum VideoPublishState { case off, starting, live, failed }

    struct Config {
        let sfuBaseUrl: String
        let jwt: String
        /// 32-byte MLS exporter seed (`Group.exportSecret("lakko/call/v1", callId, 32)`).
        ///
        /// Optional to support the ADR-0032 multi-joiner path: a link joiner
        /// connects signaling-only (`deferPublish`) and does NOT know the seed
        /// until it has joined the MLS group via the committer's Welcome. It
        /// then calls ``publish(seed:localLeaf:)`` with the derived seed. On the
        /// normal in-chat group-call path the seed is present at construction.
        let seed: Data?
        /// This device's MLS leaf index — the SFrame KID it stamps. Optional
        /// for the same ADR-0032 reason as ``seed`` (a link joiner learns it
        /// only after joining).
        let localLeaf: UInt32?
        let audio: Bool
        let video: Bool
        /// ADR-0032: when `true`, ``connect()`` opens the SFU signaling socket
        /// (so the peer can relay + receive the KeyPackage / Welcome handshake
        /// and appear on the roster) but does NOT capture or publish media. The
        /// caller publishes later via ``publish(seed:localLeaf:)`` once it holds
        /// a valid seed — the fail-closed "join media only after the Welcome"
        /// posture (invariant #4). Defaults to `false` (publish immediately, the
        /// normal in-chat path — byte-unchanged).
        var deferPublish: Bool = false
    }

    struct Callbacks {
        var onStateChange: ((State) -> Void)?
        var onWelcome: ((_ selfPeerId: String, _ roster: [SfuPeerSnapshot]) -> Void)?
        var onPeerJoined: ((_ peerId: String, _ accountPseudonym: String, _ peerTag: String) -> Void)?
        var onPeerLeft: ((_ peerId: String) -> Void)?
        /// A remote video track (with its MSID stream id = source peer
        /// id) arrived or was removed (`nil`).
        var onRemoteVideoTrack: ((RTCVideoTrack?, _ streamId: String?) -> Void)?
        /// The LOCAL camera video track became available (`config.video`
        /// publish) or was torn down (`nil`). Drives the self-preview
        /// tile.
        var onLocalVideoTrack: ((RTCVideoTrack?) -> Void)?
        /// Mid-call camera-on lifecycle (piece B). Mirrors the web
        /// `onVideoPublishState`; drives a camera-button spinner. Defaulted
        /// so existing memberwise callers keep their shorter initialiser.
        var onVideoPublishState: ((VideoPublishState) -> Void)? = nil
        var onServerError: ((SfuErrorCode, String) -> Void)?
        /// ADR-0032: an opaque peer-to-peer relay payload arrived over the SFU
        /// `signal-relay` frame. `from` is the sending peer's SFU peer id;
        /// `payload` is the decoded bytes (the SFU never inspects them —
        /// invariant #2). The multi-joiner orchestrator classifies these with
        /// `dispatchIncomingEnvelope` (CallKeyPackage / CallWelcome / commit).
        /// Defaulted so the in-chat group-call callers (which don't use the
        /// relay handshake) keep their shorter memberwise initialiser.
        var onSignalRelay: ((_ from: String, _ payload: Data) -> Void)? = nil
        /// Camera-off-cleanup: a remote peer broadcast its camera on/off
        /// state over the opaque SFU `signal-relay`. `from` is the sender's
        /// SFU peer id (== the roster tile key); `cameraOn` is the decoded
        /// flag. A remote camera OFF is a track-`isEnabled = false` MUTE that
        /// fires NO track event, so a peer can't otherwise tell — this
        /// app-level signal lets the receiver flip that participant's roster
        /// `camOn` gate deterministically (fixes both camera-off AND
        /// camera-back-on). UI-only: the SFU never inspects the payload
        /// (invariant #2); nothing about SFrame / the seed / MLS changes.
        /// Defaulted so existing memberwise callers keep their shorter
        /// initialiser.
        var onRemoteMediaState: ((_ from: String, _ cameraOn: Bool) -> Void)? = nil
        /// A remote receiver's SFrame decryptor could not be armed, so nothing
        /// that receiver carries will ever decode. `streamId` is the MSID
        /// stream group (== the source peer id on an SFU-routed call, so the
        /// roster tile key is `"peer-<streamId>"`); `kind` is `"audio"` or
        /// `"video"`.
        ///
        /// This is reported, never acted on by the session: the media is
        /// already refused fail-closed by libwebrtc, and a call is not ended
        /// because one of several peers has one dead stream. The adapter marks
        /// that participant so the screen stops passing a refusal off as a
        /// network stall. Defaulted so existing memberwise callers keep their
        /// shorter initialiser.
        var onUndecryptableReceiver: ((_ streamId: String?, _ kind: String) -> Void)? = nil
    }

    private let config: Config
    private let callbacks: Callbacks
    /// Call-key seed of the CURRENT MLS epoch (G-18, ADR-0029). Starts
    /// at `config.seed` (or `nil` on the ADR-0032 deferred-publish path,
    /// set later by ``publish(seed:localLeaf:)``) and advances on
    /// `rekey`, so a late-arriving receiver is armed at the current epoch
    /// (not the start-of-call one). Mirrors the web
    /// `SfuCallSession.activeSeed`.
    private var activeSeed: Data?
    /// This device's SFrame KID (MLS leaf). From `config.localLeaf`, or
    /// set by ``publish(seed:localLeaf:)`` on the deferred-publish path.
    private var publishLeaf: UInt32
    /// Our own camera on/off state, broadcast to the roster over the opaque
    /// SFU `signal-relay` (camera-off-cleanup). A video-from-start call is
    /// camera-on at connect; an audio-only call starts camera-off and flips
    /// on via ``publishVideo()`` / off via ``setCameraEnabled(_:)``. Peers
    /// learn every change from the broadcast, since a mute (track
    /// `isEnabled` toggle) fires no track event.
    private var localCameraOn: Bool
    private var signaling: SfuSignalingClient?
    private var pc: WebRTCPeerConnection?
    /// Local camera capturer — held for the call's lifetime so it
    /// outlives `openPeerConnectionAndPublish` and is torn down in
    /// `hangUp`. `nil` for an audio-only call (or when no camera exists).
    private var localCameraCapture: LocalCameraCapture?
    private(set) var state: State = .idle

    /// Serializes SFU-initiated offers so two `createAnswer` cycles
    /// never overlap.
    private var offerQueue: [String] = []
    private var drainingOffers = false

    // ---- Serialized mid-call renegotiation (piece B: camera-on) -----------
    // Unlike the web (POLITE peer + implicit rollback glare handling), the
    // native path SERIALIZES: our own publish / renegotiation offer completes
    // fully (offer -> answer) BEFORE any SFU subscribe-offer is processed. A
    // colliding SFU offer simply waits in `offerQueue` until our negotiation
    // finishes — no rollback, no `onnegotiationneeded`.
    //
    // `makingOffer` — true from when we send an offer until its answer lands.
    // `negotiationReady` — flips true once the FIRST publish offer is answered,
    //   so a mid-call renegotiation is never attempted before the initial
    //   publish completes.
    // `pendingRenegotiation` — a camera-on that raced an in-flight offer / a
    //   non-stable state; the completing answer / drain re-drives it.
    private var makingOffer = false
    private var negotiationReady = false
    private var pendingRenegotiation = false
    /// Mid-call camera-on lifecycle (piece B).
    private var videoPublishState: VideoPublishState = .off

    init(config: Config, callbacks: Callbacks) {
        self.config = config
        self.callbacks = callbacks
        self.activeSeed = config.seed
        self.publishLeaf = config.localLeaf ?? 0
        self.localCameraOn = config.video
    }

    /// Broadcast our current camera state to every other peer over the
    /// opaque SFU `signal-relay` (camera-off-cleanup). Best-effort — a no-op
    /// if signaling is not open. `sendSignalRelayToCall` base64s the
    /// `composeMediaState` bytes into the relay payload; the SFU forwards it
    /// verbatim without inspection (invariant #2). Called on every local
    /// camera transition, once when the call connects, and again whenever a
    /// new peer joins (so a newcomer is initial-synced deterministically).
    private func broadcastMediaState() {
        guard signaling != nil else { return }
        sendSignalRelayToCall(payload: composeMediaState(cameraOn: localCameraOn))
    }

    /// Rotate the call key on a mid-call MLS epoch change (G-18,
    /// ADR-0029). The caller re-derives `newSeed =
    /// group.exportSecret("lakko/call/v1", callId, 32)` after processing
    /// a commit that advanced the epoch and passes it here; every
    /// installed encryptor + decryptor rotates, retaining the previous
    /// key for the overlap window so in-flight frames still open, then
    /// dropping it. A late-arriving receiver is then armed at the new
    /// epoch (`activeSeed`). No-op if the seed is unchanged. Mirrors the
    /// web `SfuCallSession.rekey`.
    func rekey(newSeed: Data) {
        if let current = activeSeed, current == newSeed { return }
        activeSeed = newSeed
        pc?.rekeySFrame(seed: newSeed)
    }

    /// Open the SFU connection, publish the local audio (SFrame-sealed),
    /// and start subscribing. Throws before any media capture if the
    /// SFrame shim is unavailable (invariant #4).
    func connect() async throws {
        guard state == .idle else { throw SfuCallError.alreadyConnected }
        guard SFrameAvailability.shared.isCallingAvailable else {
            throw SfuCallError.sframeUnavailable
        }
        setState(.connecting)

        // 1. Open signaling first so the publish offer + trickled ICE
        //    have a live socket.
        let sig = SfuSignalingClient(
            baseUrl: config.sfuBaseUrl,
            jwt: config.jwt,
            callbacks: makeSignalingCallbacks()
        )
        signaling = sig
        do {
            try sig.connect()
        } catch {
            fail()
            throw error
        }

        // 2. Build the peer connection + publish — UNLESS the caller asked to
        //    defer (ADR-0032 link joiner: stay signaling-only until it holds a
        //    Welcome + a derived seed, then call `publish()`).
        if config.deferPublish { return }
        do {
            try await openPeerConnectionAndPublish()
        } catch {
            fail()
            throw error
        }
    }

    /// ADR-0032: publish local media over the already-open signaling socket,
    /// arming SFrame on the supplied (or previously-set) seed + leaf. Used by
    /// the deferred-publish path once the client has joined the MLS group and
    /// derived the per-call seed — media only ever starts AFTER a valid seed is
    /// in hand (invariant #4, no plaintext-media fallback). Idempotent: a no-op
    /// if a peer connection already exists.
    ///
    /// - Throws: `SfuCallError.sframeUnavailable` if no seed is available (never
    ///   publishes unencrypted).
    func publish(seed: Data?, localLeaf: UInt32?) async throws {
        guard pc == nil else { return }
        if let seed { activeSeed = seed }
        if let localLeaf { publishLeaf = localLeaf }
        guard activeSeed != nil else { throw SfuCallError.sframeUnavailable }
        do {
            try await openPeerConnectionAndPublish()
        } catch {
            fail()
            throw error
        }
    }

    /// ADR-0032: relay an opaque payload to one peer (the KeyPackage / Welcome
    /// handshake). No-op if signaling is not open.
    func sendSignalRelay(toPeer peerId: String, payload: Data) {
        signaling?.sendSignalRelay(toPeer: peerId, payload: payload)
    }

    /// ADR-0032: relay an opaque payload to every other peer in the call (a
    /// broadcast KeyPackage request or an MLS commit). No-op if not open.
    func sendSignalRelayToCall(payload: Data) {
        signaling?.sendSignalRelayToCall(payload: payload)
    }

    /// Mute / unmute the local microphone (local-only track gate).
    func setMicrophoneEnabled(_ on: Bool) {
        pc?.setMicrophoneEnabled(on)
    }

    /// Tear down. Idempotent.
    func hangUp() {
        // Stop the camera first (fire-and-forget — `LocalCameraCapture`
        // also stops in its deinit, but the explicit stop releases the
        // `AVCaptureSession` promptly).
        if let capture = localCameraCapture {
            localCameraCapture = nil
            callbacks.onLocalVideoTrack?(nil)
            Task { await capture.stop() }
        }
        signaling?.close()
        signaling = nil
        pc?.close()
        pc = nil
        offerQueue.removeAll()
        // Reset the serialized-renegotiation + video-publish state so a late
        // callback after teardown starts clean.
        makingOffer = false
        negotiationReady = false
        pendingRenegotiation = false
        setVideoPublishState(.off)
        if state != .failed, state != .closed {
            setState(.closed)
        }
    }

    // MARK: - Peer connection + publish

    private func openPeerConnectionAndPublish() async throws {
        guard let seed = activeSeed else {
            // Never publish media without a seed (invariant #4).
            throw SfuCallError.sframeUnavailable
        }
        let connection = WebRTCPeerConnection(
            iceServers: WebRTCPeerConnection.defaultIceServers()
        )
        connection.installDelegate(makePeerDelegate())
        pc = connection

        // Publish the local mic, and the camera when this is a video
        // call. Both tracks must be added BEFORE arming SFrame + building
        // the offer: `armSFrameEncryptors` walks the present senders, and
        // the offer's m-lines are fixed at `createOffer` time (client-
        // initiated add-track post-offer is the deferred renegotiation
        // path). So order is: add audio -> (add video) -> arm SFrame ->
        // offer, guaranteeing the SFU never sees a plaintext frame on
        // either track (invariant #2/#4).
        connection.addLocalAudioTrack(enabled: config.audio)
        if config.video {
            startLocalCameraCapture(on: connection)
        }
        guard connection.armSFrameEncryptors(seed: seed, localLeaf: publishLeaf) else {
            // A failed arm is fail-closed (invariant #4).
            throw SfuCallError.sframeUnavailable
        }
        let offer = try await connection.createOffer(audio: config.audio, video: config.video)
        // Mark our initial publish offer in flight so an SFU subscribe-offer
        // that arrives before the answer waits in `offerQueue`
        // (`drainServerOffers` guards on `makingOffer`). The first
        // `applyRemoteAnswer` clears it + arms `negotiationReady` for a
        // mid-call renegotiation (piece B: `publishVideo`).
        makingOffer = true
        signaling?.sendSdpOffer(offer)
    }

    /// Capture the local camera + add a video track to the peer
    /// connection. Mirrors `CallSession.startLocalCameraCapture`: build a
    /// fresh `RTCVideoSource`, wrap it in a `LocalCameraCapture` (front
    /// camera, 720p/30), add the backing `RTCVideoTrack` to the PC (so the
    /// upcoming offer advertises a video m-line + `armSFrameEncryptors`
    /// finds the sender), surface the track for the self-preview tile, and
    /// start the `AVCaptureSession` off the main actor (`startCapture`
    /// blocks briefly). A camera that fails to start leaves the (silent)
    /// video sender in place — the call proceeds; the receiver sees a
    /// black frame — and clears the preview track so the UI hides it.
    private func startLocalCameraCapture(on connection: WebRTCPeerConnection) {
        guard LocalCameraCapture.hasUsableCamera() else {
            NSLog("SfuCallSession: no camera available; publishing audio-only")
            return
        }
        let source = WebRTCPeerConnection.newVideoSource()
        guard let capture = LocalCameraCapture(source: source) else {
            NSLog("SfuCallSession: LocalCameraCapture init returned nil")
            return
        }
        connection.addLocalVideoTrack(source)
        localCameraCapture = capture
        callbacks.onLocalVideoTrack?(connection.currentLocalVideoTrack())
        Task { [weak self] in
            do {
                try await capture.start()
            } catch {
                NSLog("SfuCallSession: camera start failed: %@", String(describing: error))
                await MainActor.run {
                    self?.localCameraCapture = nil
                    self?.callbacks.onLocalVideoTrack?(nil)
                }
            }
        }
    }

    /// Enable / disable the local camera track (local-only gate). Meaningful
    /// once a video sender exists — either a `config.video` call (added at
    /// connect) or after a mid-call ``publishVideo()``. Toggling without a
    /// track is a no-op.
    func setCameraEnabled(_ on: Bool) {
        pc?.setCameraEnabled(on)
        // Camera-off-cleanup: a mute (track `isEnabled` toggle) fires no
        // track event on the receiver, so broadcast the state change so
        // peers flip our roster `camOn` gate. Covers camera OFF (the in-chat
        // toggle's off path) and any explicit re-enable.
        localCameraOn = on
        broadcastMediaState()
    }

    /// Current mid-call camera-on lifecycle state (piece B).
    func currentVideoPublishState() -> VideoPublishState { videoPublishState }

    /// Turn the camera ON mid-call (piece B). When the call started audio-only
    /// there is no video sender, so this ACQUIRES the camera, adds the track
    /// (SFrame-sealed like the audio sender), and renegotiates. Idempotent +
    /// re-entrant-safe:
    ///   * a live video sender already exists -> just re-enable its track;
    ///   * a capture is already in flight       -> no-op (do not double-acquire).
    /// Fail-safe (invariant #4): on a camera / attach failure the capture is
    /// torn down (never a half-added / unsealed sender reaches the SFU) and the
    /// state goes `.failed`, so the caller reverts the toggle + surfaces a
    /// message; the call stays up audio-only.
    ///
    /// - Throws: on capture failure (no camera / start failure) or an SFrame
    ///   arm failure, so the caller can distinguish it from "already publishing".
    func publishVideo() async throws {
        guard let connection = pc, activeSeed != nil else {
            // No peer connection yet (a deferred-publish joiner that has not
            // joined). Nothing to publish — not an error.
            return
        }
        // Idempotent: a live video sender already exists (a video-from-start
        // call, or a prior publishVideo) — just re-enable. No capture, no
        // renegotiation.
        if connection.currentLocalVideoTrack() != nil {
            connection.setCameraEnabled(true)
            setVideoPublishState(.live)
            // Camera-back-on is a track re-enable (no track event fires on the
            // receiver), so broadcast so peers un-hide our tile.
            localCameraOn = true
            broadcastMediaState()
            return
        }
        // In-flight guard — a capture is already running; do not double-acquire.
        if videoPublishState == .starting { return }
        setVideoPublishState(.starting)

        // Acquire the camera FIRST, before adding any track, so a capture
        // failure never leaves a half-added sender on the peer connection.
        guard LocalCameraCapture.hasUsableCamera() else {
            setVideoPublishState(.failed)
            throw SfuCallError.noCamera
        }
        let source = WebRTCPeerConnection.newVideoSource()
        guard let capture = LocalCameraCapture(source: source) else {
            setVideoPublishState(.failed)
            throw SfuCallError.noCamera
        }
        do {
            try await capture.start()
        } catch {
            setVideoPublishState(.failed)
            throw error
        }

        // Re-read the peer connection + seed AFTER the async capture — a rekey
        // or teardown may have happened while the camera was starting; arm at
        // the CURRENT epoch seed on the CURRENT peer connection.
        guard let liveConn = pc, let seed = activeSeed else {
            await capture.stop()
            setVideoPublishState(.off)
            return
        }
        // Add the video track, then arm SFrame on ONLY the new video sender
        // (invariant #4 — the mid-call video sender is SFrame-sealed exactly
        // like the audio sender; `armSFrameVideoEncryptor` does NOT touch the
        // already-armed audio encryptor). On an arm failure tear the camera
        // down and do NOT renegotiate, so an unsealed video m-line is never
        // offered to the SFU (fail-closed).
        liveConn.addLocalVideoTrack(source)
        localCameraCapture = capture
        guard liveConn.armSFrameVideoEncryptor(seed: seed, localLeaf: publishLeaf) else {
            await capture.stop()
            localCameraCapture = nil
            callbacks.onLocalVideoTrack?(nil)
            setVideoPublishState(.failed)
            throw SfuCallError.sframeUnavailable
        }
        // Self-preview tile.
        callbacks.onLocalVideoTrack?(liveConn.currentLocalVideoTrack())
        setVideoPublishState(.live)
        // Camera-off-cleanup: announce camera-on. The fresh track also
        // reaches peers via the renegotiation below (redundant but robust);
        // the broadcast is what carries later mute/unmute transitions.
        localCameraOn = true
        broadcastMediaState()
        // Renegotiate so the SFU (and thus other peers) learn the new video
        // m-line. Serialized behind any in-flight offer.
        await driveRenegotiationOffer()
    }

    private func setVideoPublishState(_ next: VideoPublishState) {
        guard videoPublishState != next else { return }
        videoPublishState = next
        callbacks.onVideoPublishState?(next)
    }

    /// Send a client-initiated renegotiation offer (piece B: a camera track was
    /// added mid-call). Serialized — no glare rollback: only start an offer
    /// from a clean `.stable` state with no in-flight offer / mid-drain answer.
    /// Otherwise DEFER (`pendingRenegotiation`) — the completing
    /// `applyRemoteAnswer` / `drainServerOffers` re-drives us. `makingOffer`
    /// stays true until the SFU's answer arrives.
    private func driveRenegotiationOffer() async {
        guard let connection = pc, let signaling = signaling else { return }
        if !negotiationReady || makingOffer || drainingOffers
            || connection.signalingState != .stable {
            pendingRenegotiation = true
            return
        }
        pendingRenegotiation = false
        makingOffer = true
        do {
            let offer = try await connection.createRenegotiationOffer()
            signaling.sendSdpOffer(offer)
            // `makingOffer` stays true until `applyRemoteAnswer` clears it.
        } catch {
            makingOffer = false
            // A failed renegotiation leaves the call up as-is; the local
            // self-preview is already live. A later stable transition re-drives
            // any still-pending change.
        }
    }

    // MARK: - Signaling

    private func makeSignalingCallbacks() -> SfuSignalingCallbacks {
        var cb = SfuSignalingCallbacks()
        // Every inbound frame this path depends on prints one line. The live
        // group-call investigation that added them spent a run on inference
        // because the SFU path was silent end to end: no welcome line, no
        // receiver line, and — worst — no line at all for a frame the parser
        // rejected, since `onParseError` had no subscriber anywhere in the app.
        // Absence of a log meant nothing, which is the one thing a log must
        // never mean.
        cb.onParseError = { raw, error in
            NSLog(
                "SfuCallSession: signaling frame PARSE FAILED err=%@ text=%@",
                "\(error)", String(raw.prefix(300)))
        }
        cb.onWelcome = { [weak self] selfPeerId, callId, roster in
            // The joiner learns about everyone already in the call HERE, not
            // from peer-joined — a second joiner gets no peer-joined for the
            // peers that preceded it. `rosterCount` is therefore the whole
            // answer to "did this device learn who else is on the call".
            NSLog(
                "SfuCallSession: welcome self=%@ call=%@ rosterCount=%d peers=%@",
                selfPeerId, callId, roster.count,
                roster.map(\.peerId).joined(separator: ","))
            Task { @MainActor in self?.callbacks.onWelcome?(selfPeerId, roster) }
        }
        cb.onPeerJoined = { [weak self] peerId, pseudonym, peerTag in
            NSLog("SfuCallSession: peer joined peer=%@", peerId)
            Task { @MainActor in
                guard let self else { return }
                self.callbacks.onPeerJoined?(peerId, pseudonym, peerTag)
                // Camera-off-cleanup: re-announce our current camera state so
                // the newcomer (which was not in our Welcome-roster broadcast)
                // is initial-synced deterministically.
                self.broadcastMediaState()
            }
        }
        cb.onPeerLeft = { [weak self] peerId in
            NSLog("SfuCallSession: peer left peer=%@", peerId)
            Task { @MainActor in self?.callbacks.onPeerLeft?(peerId) }
        }
        cb.onSdpAnswer = { [weak self] sdp in
            Task { @MainActor in await self?.applyRemoteAnswer(sdp) }
        }
        cb.onSdpOffer = { [weak self] sdp in
            // The SFU offers when it has a track to give us, so this line is
            // the difference between "nobody sent us anything to subscribe to"
            // and "we were offered a stream and did not take it".
            NSLog("SfuCallSession: server offer received bytes=%d", sdp.count)
            Task { @MainActor in self?.enqueueServerOffer(sdp) }
        }
        cb.onServerError = { [weak self] code, detail in
            // Logged here rather than left to the adapter, which discards it.
            NSLog("SfuCallSession: server error code=%@ detail=%@", "\(code)", detail)
            Task { @MainActor in self?.callbacks.onServerError?(code, detail) }
        }
        cb.onSignalRelay = { [weak self] from, payloadB64 in
            Task { @MainActor in self?.dispatchSignalRelay(from: from, payloadB64: payloadB64) }
        }
        cb.onClosed = { [weak self] in
            Task { @MainActor in self?.onSignalingClosed() }
        }
        return cb
    }

    /// ADR-0032 + camera-off-cleanup: decode the opaque relay payload
    /// (base64url), classify it, and dispatch. A camera on/off media-state
    /// signal is handled in-session (it fires `onRemoteMediaState`, which the
    /// in-chat `SfuCallUi` maps onto the participant's roster `camOn` gate);
    /// every other envelope is forwarded verbatim to `onSignalRelay` (the
    /// multi-joiner handshake dispatcher, which re-classifies it). A malformed
    /// payload is dropped, never fatal (mirror of the web `onSignalRelayFrame`,
    /// which uses the same `sfuDecodePayload`).
    private func dispatchSignalRelay(from: String, payloadB64: String) {
        guard let payload = try? sfuDecodePayload(payloadB64: payloadB64) else { return }
        if let env = try? dispatchIncomingEnvelope(plaintext: payload),
            env.kind == .mediaState, let cameraOn = env.cameraOn {
            callbacks.onRemoteMediaState?(from, cameraOn)
            return
        }
        callbacks.onSignalRelay?(from, payload)
    }

    private func applyRemoteAnswer(_ sdp: String) async {
        guard let pc = pc else { return }
        do {
            try await pc.applyRemoteAnswer(sdp)
        } catch {
            fail()
            return
        }
        // The initial publish (or a mid-call renegotiation) is now answered:
        // clear the in-flight flag + arm renegotiation for future track
        // changes (piece B). Then either drive a camera-on that queued while we
        // were mid-offer, or drain any SFU subscribe-offers that waited.
        makingOffer = false
        negotiationReady = true
        if pendingRenegotiation {
            await driveRenegotiationOffer()
        } else {
            await drainServerOffers()
        }
    }

    private func enqueueServerOffer(_ sdp: String) {
        offerQueue.append(sdp)
        Task { @MainActor in await self.drainServerOffers() }
    }

    private func drainServerOffers() async {
        if drainingOffers { return }
        // Our own publish / renegotiation offer is in flight — leave SFU offers
        // queued; `applyRemoteAnswer` re-drains once the answer clears
        // `makingOffer` (serialize: our offer completes before we answer an
        // SFU offer, so two `setRemoteDescription` cycles never overlap).
        if makingOffer { return }
        drainingOffers = true
        while !offerQueue.isEmpty {
            let sdp = offerQueue.removeFirst()
            guard let pc = pc, let signaling = signaling else { break }
            do {
                let answer = try await pc.createAnswer(remoteOfferSdp: sdp)
                signaling.sendSdpAnswer(answer)
                NSLog("SfuCallSession: answered server offer bytes=%d", answer.count)
            } catch {
                // A single bad renegotiation drops that subscription; the call
                // stays up. Continue draining — but say so: dropping one
                // subscription silently is indistinguishable, from the outside,
                // from a peer that never published.
                NSLog("SfuCallSession: server offer answer FAILED err=%@", "\(error)")
            }
        }
        drainingOffers = false
        // A camera-on that raced an SFU offer queued itself as pending — drive
        // it now that the queue is drained + the state is back to stable.
        if pendingRenegotiation {
            await driveRenegotiationOffer()
        }
    }

    private func onSignalingClosed() {
        NSLog("SfuCallSession: signaling socket closed state=%@", "\(state)")
        if state == .closed || state == .failed { return }
        fail()
    }

    // MARK: - Peer connection delegate

    private func makePeerDelegate() -> WebRTCPeerConnection.Delegate {
        WebRTCPeerConnection.Delegate(
            onLocalCandidate: { [weak self] _, _, candidate in
                self?.signaling?.sendIceCandidate(candidate)
            },
            onConnectionState: { [weak self] rtcState in
                self?.onConnectionState(rtcState)
            },
            onRemoteVideoTrack: { [weak self] track, streamId in
                self?.callbacks.onRemoteVideoTrack?(track, streamId)
            },
            onRemoteReceiver: { [weak self] receiver, streamId in
                // Arm the SFrame decryptor on every incoming track
                // (audio + video) — invariant #4. Use the CURRENT epoch
                // seed (`activeSeed`), so a receiver that arrives after a
                // mid-call rekey is armed at the new epoch (G-18). A remote
                // track only arrives after this client published (it has a
                // peer connection), so the seed is always set here.
                // A torn-down peer connection is a hang-up racing a receiver,
                // not a decryption fault — there is no longer a call to report
                // it on, and accusing a peer on the way out would be a lie of
                // its own.
                guard let self = self, let seed = self.activeSeed, let pc = self.pc else {
                    return
                }
                let kind = receiver.track?.kind ?? "unknown"
                guard !pc.armSFrameDecryptor(for: receiver, seed: seed) else {
                    // Logged on SUCCESS as well as failure. While only failures
                    // printed, a silent path could mean "armed fine" or "never
                    // fired at all", and a live investigation read the silence
                    // as the second when it may well have been the first.
                    NSLog(
                        "SfuCallSession: SFrame decryptor armed: kind=%@ peer=%@",
                        kind, streamId ?? "unknown")
                    return
                }
                // A receiver with no decryptor decodes nothing: the crypto
                // options declare frame encryption required, so libwebrtc
                // discards every frame instead of rendering it. That is the
                // right refusal and it is not in question here — what is in
                // question is that it looks EXACTLY like a peer with a bad
                // uplink. The result used to be discarded, which made a fault
                // that kills one peer's media indistinguishable from weather.
                //
                // It is not escalated to ending the call: the arm is per
                // receiver, so one failure is one stream of one peer, and
                // tearing down an N-party call (or hiding the peer) would
                // destroy the very evidence the user needs. It is reported
                // instead, and the screen says so.
                NSLog(
                    "SfuCallSession: SFrame decryptor arm FAILED: kind=%@ peer=%@",
                    kind, streamId ?? "unknown")
                self.callbacks.onUndecryptableReceiver?(streamId, kind)
            }
        )
    }

    private func onConnectionState(_ rtcState: RTCPeerConnectionState) {
        switch rtcState {
        case .connected:
            if state == .connecting { setState(.connected) }
            // Camera-off-cleanup: announce our initial camera state to the
            // roster now that the media path is up, so an already-present peer
            // renders us correctly (a peer joining camera-off is shown off).
            broadcastMediaState()
        case .failed, .disconnected, .closed:
            fail()
        default:
            break
        }
    }

    // MARK: - State

    private func setState(_ next: State) {
        state = next
        callbacks.onStateChange?(next)
    }

    private func fail() {
        if state == .failed || state == .closed { return }
        setState(.failed)
        hangUp()
    }
}
