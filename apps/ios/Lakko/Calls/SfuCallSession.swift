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

    enum SfuCallError: Error { case alreadyConnected, sframeUnavailable }

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
        var onServerError: ((SfuErrorCode, String) -> Void)?
        /// ADR-0032: an opaque peer-to-peer relay payload arrived over the SFU
        /// `signal-relay` frame. `from` is the sending peer's SFU peer id;
        /// `payload` is the decoded bytes (the SFU never inspects them —
        /// invariant #2). The multi-joiner orchestrator classifies these with
        /// `dispatchIncomingEnvelope` (CallKeyPackage / CallWelcome / commit).
        /// Defaulted so the in-chat group-call callers (which don't use the
        /// relay handshake) keep their shorter memberwise initialiser.
        var onSignalRelay: ((_ from: String, _ payload: Data) -> Void)? = nil
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

    init(config: Config, callbacks: Callbacks) {
        self.config = config
        self.callbacks = callbacks
        self.activeSeed = config.seed
        self.publishLeaf = config.localLeaf ?? 0
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

    /// Enable / disable the local camera track (local-only gate — only
    /// meaningful when the call was started with `config.video`, since the
    /// video track is added at connect; toggling without a track is a
    /// no-op).
    func setCameraEnabled(_ on: Bool) {
        pc?.setCameraEnabled(on)
    }

    // MARK: - Signaling

    private func makeSignalingCallbacks() -> SfuSignalingCallbacks {
        var cb = SfuSignalingCallbacks()
        cb.onWelcome = { [weak self] selfPeerId, _, roster in
            Task { @MainActor in self?.callbacks.onWelcome?(selfPeerId, roster) }
        }
        cb.onPeerJoined = { [weak self] peerId, pseudonym, peerTag in
            Task { @MainActor in self?.callbacks.onPeerJoined?(peerId, pseudonym, peerTag) }
        }
        cb.onPeerLeft = { [weak self] peerId in
            Task { @MainActor in self?.callbacks.onPeerLeft?(peerId) }
        }
        cb.onSdpAnswer = { [weak self] sdp in
            Task { @MainActor in await self?.applyRemoteAnswer(sdp) }
        }
        cb.onSdpOffer = { [weak self] sdp in
            Task { @MainActor in self?.enqueueServerOffer(sdp) }
        }
        cb.onServerError = { [weak self] code, detail in
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

    /// ADR-0032: decode the opaque relay payload (base64url) and hand it to the
    /// caller. A malformed relay payload is dropped, never fatal (mirror of the
    /// web `onSignalRelayFrame`, which uses the same `sfuDecodePayload`).
    private func dispatchSignalRelay(from: String, payloadB64: String) {
        guard let cb = callbacks.onSignalRelay else { return }
        guard let payload = try? sfuDecodePayload(payloadB64: payloadB64) else { return }
        cb(from, payload)
    }

    private func applyRemoteAnswer(_ sdp: String) async {
        guard let pc = pc else { return }
        do {
            try await pc.applyRemoteAnswer(sdp)
        } catch {
            fail()
        }
    }

    private func enqueueServerOffer(_ sdp: String) {
        offerQueue.append(sdp)
        Task { @MainActor in await self.drainServerOffers() }
    }

    private func drainServerOffers() async {
        if drainingOffers { return }
        drainingOffers = true
        defer { drainingOffers = false }
        while !offerQueue.isEmpty {
            let sdp = offerQueue.removeFirst()
            guard let pc = pc, let signaling = signaling else { break }
            do {
                let answer = try await pc.createAnswer(remoteOfferSdp: sdp)
                signaling.sendSdpAnswer(answer)
            } catch {
                // A single bad renegotiation drops that subscription;
                // the call stays up. Continue draining.
            }
        }
    }

    private func onSignalingClosed() {
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
            onRemoteReceiver: { [weak self] receiver, _ in
                // Arm the SFrame decryptor on every incoming track
                // (audio + video) — invariant #4. Use the CURRENT epoch
                // seed (`activeSeed`), so a receiver that arrives after a
                // mid-call rekey is armed at the new epoch (G-18). A remote
                // track only arrives after this client published (it has a
                // peer connection), so the seed is always set here.
                guard let self = self, let seed = self.activeSeed else { return }
                _ = self.pc?.armSFrameDecryptor(for: receiver, seed: seed)
            }
        )
    }

    private func onConnectionState(_ rtcState: RTCPeerConnectionState) {
        switch rtcState {
        case .connected:
            if state == .connecting { setState(.connected) }
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
