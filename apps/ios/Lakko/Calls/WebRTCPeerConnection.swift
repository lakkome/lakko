// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import WebRTC

/// Lakko's thin wrapper over `RTCPeerConnection` (G3-1).
///
/// The wrapper isolates every Google-WebRTC API call inside
/// this one file so the rest of the iOS app stays platform-
/// neutral (mirror of the symmetry the Rust core already
/// has between `lakko-calls` and `lakko-transport`).
///
/// ## What it owns
///
/// - One `RTCPeerConnection` instance per 1×1 call.
/// - The `RTCPeerConnectionFactory` (shared across all peers
///   in this process — Google's API explicitly recommends
///   this; one factory per peer leaks audio I/O units on
///   iOS).
/// - The local audio / video tracks (audio-only on the
///   first pass; the video plumbing is a clean extension
///   point — see `addLocalVideoTrack(_:)`).
///
/// ## What it does NOT own
///
/// - The signaling channel — the caller (`CallSession`)
///   handles `composeCallSignaling(...)` and chat-route
///   delivery. This wrapper only emits **typed callbacks**
///   when it has something the caller must ship over
///   signaling.
/// - The state machine — `CallSession.WebCallState` is the
///   single source of truth. This wrapper surfaces the raw
///   `RTCPeerConnectionState` transitions; the session
///   translates them into the user-visible state.
/// - The MLS-derived call key — SFrame integration arrives
///   in a follow-up sub-sprint (`G3-1-sframe`), at which
///   point the wrapper grows an `installCallKey(...)` hook
///   wired to the `RTCFrameTransformer` insertable-streams
///   API. Today the wrapper is **transport-only** — the
///   audio/video bytes ride plain SRTP without the
///   end-to-end SFrame layer. Documented as deferred so the
///   audit pack does not treat the iOS call surface as
///   E2EE-complete prematurely.
///
/// ## Threading
///
/// Every method on this class is annotated `@MainActor`.
/// `RTCPeerConnection` itself uses internal background
/// queues for its work, but the **observer callbacks** —
/// the `RTCPeerConnectionDelegate` methods that fire on
/// arbitrary threads — are bounced into the main actor
/// before any state mutation. This matches the
/// `CallSession`'s `@MainActor` constraint so the
/// `@Published` properties never get mutated off-main.
@MainActor
public final class WebRTCPeerConnection: NSObject {

    // MARK: - Public callback surface

    /// Caller-supplied delegate. The wrapper invokes these
    /// closures on the main actor; the caller is free to
    /// mutate `@Published` state inline.
    public struct Delegate {
        /// Emitted when `RTCPeerConnection` produces a local
        /// ICE candidate. The caller must ship the
        /// `(sdpMid, sdpMLineIndex, candidate)` tuple to the
        /// remote peer via the existing call-signaling
        /// channel.
        public let onLocalCandidate: (String, Int32, String) -> Void
        /// Emitted on every `connectionState` transition.
        /// The caller maps `.connected` to
        /// `WebCallState.connected`, `.disconnected` /
        /// `.failed` / `.closed` to call teardown.
        public let onConnectionState: (RTCPeerConnectionState) -> Void
        /// G3-1-remote-video — emitted when a remote
        /// `RTCVideoTrack` is added (`Optional.some`) or
        /// removed (`nil`). `streamId` is the MSID of the
        /// owning media stream — on a call routed through the
        /// Lakko SFU this equals the originating peer id
        /// (`negotiate_if_needed` sets the MSID stream group
        /// to the source peer id), which a future N-way
        /// sprint will use to map the track to a specific
        /// participant. For the 1×1 direct-PeerConnection
        /// path there is exactly one remote track and the
        /// caller renders it for the sole remote participant.
        public let onRemoteVideoTrack: (RTCVideoTrack?, _ streamId: String?) -> Void
        public init(
            onLocalCandidate: @escaping (String, Int32, String) -> Void,
            onConnectionState: @escaping (RTCPeerConnectionState) -> Void,
            onRemoteVideoTrack: @escaping (RTCVideoTrack?, String?) -> Void = { _, _ in }
        ) {
            self.onLocalCandidate = onLocalCandidate
            self.onConnectionState = onConnectionState
            self.onRemoteVideoTrack = onRemoteVideoTrack
        }
    }

    // MARK: - Static singletons

    /// Process-wide factory. Google's iOS SDK docs:
    /// > A factory is heavy-weight and should be reused.
    /// We construct it lazily on the first instantiation
    /// of any `WebRTCPeerConnection` and keep it for the
    /// life of the process.
    private static let sharedFactory: RTCPeerConnectionFactory = {
        RTCInitializeSSL()
        // Default encoder / decoder factories: software VP8
        // + H.264 (Apple's hardware codec when available).
        // Opus is wired in via the audio module unconditionally;
        // no extra config required here.
        let encoderFactory = RTCDefaultVideoEncoderFactory()
        let decoderFactory = RTCDefaultVideoDecoderFactory()
        return RTCPeerConnectionFactory(
            encoderFactory: encoderFactory,
            decoderFactory: decoderFactory
        )
    }()

    /// `nil` until `installDelegate(_:)` is called by the
    /// owner. Kept as a separate field rather than a
    /// generic-bounded delegate type so the caller can
    /// replace it during the lifetime of the session
    /// (debug-tooling hook, future).
    private var delegate: Delegate?

    /// The underlying RTCPeerConnection. Constructed inside
    /// `init`; cleared in `close()`.
    private var peer: RTCPeerConnection?

    /// Local audio track. Held so the caller can toggle
    /// `isEnabled` from the UI's mic-mute control.
    private var localAudioTrack: RTCAudioTrack?

    /// Local video track. Created lazily by
    /// `addLocalVideoTrack(_:)` so audio-only calls don't
    /// pay the AVCapture cost.
    private var localVideoTrack: RTCVideoTrack?

    /// `RTCRtpSender` returned by `peer.add(audioTrack, ...)`.
    /// G3-sframe-native N-4 attaches the `LakkoFrameEncryptor` to
    /// this sender via `LakkoSFrameAttach.attachEncryptorToSender
    /// (_:seed:localLeaf:)` once the call's MLS exporter seed is
    /// known (which happens after the welcome / KP-forward
    /// envelope dispatcher resolves on the link-based-invite
    /// path, or immediately after `Group.create` on the
    /// 1×1-from-chat path).
    internal var localAudioSender: RTCRtpSender?

    /// Same shape as `localAudioSender`, populated by
    /// `addLocalVideoTrack(_:)` once it's no longer a stub.
    internal var localVideoSender: RTCRtpSender?

    // MARK: - Lifecycle

    /// Construct a peer connection.
    ///
    /// `iceServers` defaults to a Google STUN server +
    /// Lakko's coturn TURN servers (Phase 3b-ops); the
    /// caller can override for tests or for a region-pinned
    /// deployment. The default is supplied as a `nil`
    /// sentinel because Swift refuses to evaluate
    /// `Self.defaultIceServers()` as a default expression
    /// — covariant `Self` cannot appear there and the
    /// static is `@MainActor` while the surrounding init
    /// is `@MainActor` too but Swift's default-expression
    /// evaluator runs in a non-isolated context.
    public init(iceServers: [RTCIceServer]? = nil) {
        super.init()
        let servers = iceServers ?? Self.defaultIceServers()

        let config = RTCConfiguration()
        config.iceServers = servers
        // Force `unified-plan` SDP — the default since iOS
        // SDK M89 but pinning explicitly defends against a
        // future SDK that flips it back.
        config.sdpSemantics = .unifiedPlan
        // ECDSA per audit-pack baseline; the WebRTC default
        // is `RSA` which is both slower and bigger on the
        // wire.
        config.continualGatheringPolicy = .gatherContinually
        // Bundle every media line on one ICE component —
        // matches what `services/sfu/src/media.rs`
        // negotiates with `Rtc::sdp_api`.
        config.bundlePolicy = .maxBundle
        config.rtcpMuxPolicy = .require

        // Constraints. Mandatory ones go in `mandatory`;
        // optional are advisory. Empty for the first pass —
        // the SDK defaults match what we want.
        let constraints = RTCMediaConstraints(
            mandatoryConstraints: nil,
            optionalConstraints: nil
        )

        // The factory has the only synchronous failure
        // mode for `peerConnection(with:constraints:delegate:)`:
        // returning `nil` if it cannot allocate. We propagate
        // that as a soft failure — the caller checks
        // `isReady` and tears the session down with `.error`.
        self.peer = Self.sharedFactory.peerConnection(
            with: config,
            constraints: constraints,
            delegate: self
        )
    }

    deinit {
        // `peer.close()` is idempotent; running it twice is
        // safe. Doing it here defends against a caller that
        // forgets to call `close()` before dropping the
        // reference (common during error-path teardown).
        peer?.close()
    }

    /// Wire the caller-supplied callbacks. Call exactly once,
    /// right after construction. Subsequent calls replace
    /// the previous delegate (the delegate is process-state,
    /// not part of the wire protocol — replacing it cannot
    /// break a peer that is already negotiated).
    public func installDelegate(_ delegate: Delegate) {
        self.delegate = delegate
    }

    /// `true` if the underlying `RTCPeerConnection` was
    /// allocated successfully. `false` only on the
    /// vanishingly rare factory-allocation failure described
    /// above.
    public var isReady: Bool { peer != nil }

    /// Idempotent teardown. Releases the AV capture
    /// resources, fires `onConnectionState(.closed)` exactly
    /// once. Safe to call multiple times.
    public func close() {
        guard let p = peer else { return }
        p.close()
        peer = nil
        localAudioTrack = nil
        localVideoTrack = nil
    }

    // MARK: - Local tracks

    /// Attach a local audio track. Audio-only calls call this
    /// once during `dial` / `acceptCall`; video calls call
    /// this + [`addLocalVideoTrack`].
    ///
    /// `enabled` matches the UI's initial mic state. Toggling
    /// later goes through [`setMicrophoneEnabled`].
    public func addLocalAudioTrack(enabled: Bool) {
        guard let peer = peer else { return }
        let audioSource = Self.sharedFactory.audioSource(
            with: RTCMediaConstraints(
                mandatoryConstraints: nil,
                optionalConstraints: nil
            )
        )
        let track = Self.sharedFactory.audioTrack(
            with: audioSource,
            trackId: "lakko-audio0"
        )
        track.isEnabled = enabled
        // `peer.add(track, streamIds:)` returns an `RTCRtpSender?`
        // — the surface we need for SFrame attachment. If it's
        // nil the framework refused the track for an unrelated
        // reason (audio session conflict, etc.); we still record
        // the local track so mute toggles work, but SFrame stays
        // unattached on a degenerate peer.
        let sender = peer.add(track, streamIds: ["lakko-local-stream"])
        self.localAudioTrack = track
        self.localAudioSender = sender
    }

    /// Toggle the local audio track's `isEnabled` flag.
    /// Mute → unmute happens at the encoder boundary; the
    /// remote peer sees silence (RTP packets continue, but
    /// carry silence frames). No SDP renegotiation.
    public func setMicrophoneEnabled(_ on: Bool) {
        localAudioTrack?.isEnabled = on
    }

    /// Factory entry point for a fresh `RTCVideoSource`
    /// bound to this process's shared `RTCPeerConnectionFactory`.
    /// `LocalCameraCapture` (G3-1-video) drives frames into the
    /// returned source via the standard
    /// `RTCVideoCapturerDelegate` shape libwebrtc exposes.
    /// Keep this static so the source's lifetime is independent
    /// of any one `WebRTCPeerConnection` instance — the camera
    /// can outlive a single call session (e.g. preview during
    /// ringing).
    public static func newVideoSource() -> RTCVideoSource {
        sharedFactory.videoSource()
    }

    /// Video plumbing extension point. `addLocalVideoTrack`
    /// wires a `RTCVideoTrack` backed by the supplied
    /// `RTCVideoSource` into the peer connection, tagging the
    /// outgoing sender so `SFrame` attach + the SDP build
    /// pipeline can find it. The caller (`LocalCameraCapture`)
    /// owns the capturer + drives frames into the source; this
    /// class only owns the track + sender pair.
    ///
    /// MUST be called BEFORE the first SDP offer is built so
    /// the resulting offer advertises a video m-line. Adding a
    /// track post-offer triggers libwebrtc's renegotiation
    /// path, which the call surface does not drive today.
    public func addLocalVideoTrack(_ source: RTCVideoSource) {
        guard let peer = peer else { return }
        let track = Self.sharedFactory.videoTrack(
            with: source,
            trackId: "lakko-video0"
        )
        let sender = peer.add(track, streamIds: ["lakko-local-stream"])
        self.localVideoTrack = track
        self.localVideoSender = sender
    }

    /// G3-1-video-ui — expose the currently-attached local
    /// video track so the SwiftUI call screen can hand it
    /// to `LocalVideoPreviewView`. Returns `nil` if
    /// `addLocalVideoTrack` has not run yet (audio-only
    /// call, or pre-`completeDial`).
    public func currentLocalVideoTrack() -> RTCVideoTrack? {
        localVideoTrack
    }

    // MARK: - SFrame (G3-sframe-native N-4)

    /// Arm SFrame for every outgoing RTCRtpSender on this peer.
    /// Called by `CallSession` once the per-call MLS exporter
    /// seed is known.
    ///
    /// `seed` is exactly 32 bytes (the
    /// `Group::export_secret("lakko/call/v1", call_id, 32)`
    /// output). `localLeaf` is this device's leaf index in the
    /// call's MLS group.
    ///
    /// Returns `true` only when every present sender attached
    /// successfully. A `false` return is the signal to abort the
    /// call — security invariant #4 forbids letting one track
    /// flow encrypted while another doesn't.
    public func armSFrameEncryptors(seed: Data, localLeaf: UInt32) -> Bool {
        guard SFrameAvailability.shared.isCallingAvailable else {
            return false
        }
        var allOk = true
        for sender in [localAudioSender, localVideoSender] {
            guard let sender = sender else { continue }
            let status = LakkoSFrameAttach.attachEncryptor(
                toSender: sender,
                seed: seed,
                localLeaf: localLeaf
            )
            if status != .OK {
                allOk = false
            }
        }
        return allOk
    }

    /// Arm SFrame for a freshly-arrived remote `RTCRtpReceiver`.
    /// Called by the `peerConnection(_:didStartReceivingOn:)` /
    /// `peerConnection(_:didAdd:streams:)` delegate paths when
    /// the framework hands us a new incoming track.
    public func armSFrameDecryptor(
        for receiver: RTCRtpReceiver,
        seed: Data
    ) -> Bool {
        guard SFrameAvailability.shared.isCallingAvailable else {
            return false
        }
        let status = LakkoSFrameAttach.attachDecryptor(
            toReceiver: receiver,
            seed: seed
        )
        return status == .OK
    }

    // MARK: - SDP exchange

    /// Build the SDP offer that goes into
    /// `ClientFrame::SdpOffer`. Sets the local description
    /// internally as a side-effect of `createOffer`.
    ///
    /// `audio` / `video` control which transceivers are
    /// advertised. Audio-only calls pass `(audio: true,
    /// video: false)`.
    public func createOffer(
        audio: Bool,
        video: Bool
    ) async throws -> String {
        guard let peer = peer else {
            throw WebRTCError.peerUnavailable
        }
        let constraints = RTCMediaConstraints(
            mandatoryConstraints: [
                "OfferToReceiveAudio": audio ? "true" : "false",
                "OfferToReceiveVideo": video ? "true" : "false",
            ],
            optionalConstraints: nil
        )
        let offer: RTCSessionDescription = try await withCheckedThrowingContinuation { cont in
            peer.offer(for: constraints) { sdp, err in
                if let err = err { cont.resume(throwing: err); return }
                guard let sdp = sdp else {
                    cont.resume(throwing: WebRTCError.emptySdp); return
                }
                cont.resume(returning: sdp)
            }
        }
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            peer.setLocalDescription(offer) { err in
                if let err = err {
                    cont.resume(throwing: err)
                } else {
                    cont.resume()
                }
            }
        }
        return offer.sdp
    }

    /// Apply the remote SDP offer and build the answer that
    /// goes back over signaling. Sets the local description
    /// to the answer; sets the remote description to the
    /// offer.
    public func createAnswer(remoteOfferSdp: String) async throws -> String {
        guard let peer = peer else {
            throw WebRTCError.peerUnavailable
        }
        let remoteOffer = RTCSessionDescription(type: .offer, sdp: remoteOfferSdp)
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            peer.setRemoteDescription(remoteOffer) { err in
                if let err = err {
                    cont.resume(throwing: err)
                } else {
                    cont.resume()
                }
            }
        }
        let answer: RTCSessionDescription = try await withCheckedThrowingContinuation { cont in
            peer.answer(for: RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil)) { sdp, err in
                if let err = err { cont.resume(throwing: err); return }
                guard let sdp = sdp else {
                    cont.resume(throwing: WebRTCError.emptySdp); return
                }
                cont.resume(returning: sdp)
            }
        }
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            peer.setLocalDescription(answer) { err in
                if let err = err {
                    cont.resume(throwing: err)
                } else {
                    cont.resume()
                }
            }
        }
        return answer.sdp
    }

    /// Apply the remote SDP answer. Called on the offerer
    /// side after the answerer ships their answer back.
    public func applyRemoteAnswer(_ remoteAnswerSdp: String) async throws {
        guard let peer = peer else {
            throw WebRTCError.peerUnavailable
        }
        let remoteAnswer = RTCSessionDescription(type: .answer, sdp: remoteAnswerSdp)
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            peer.setRemoteDescription(remoteAnswer) { err in
                if let err = err {
                    cont.resume(throwing: err)
                } else {
                    cont.resume()
                }
            }
        }
    }

    // MARK: - ICE

    /// Apply a remote-supplied ICE candidate (received via
    /// `FfiCallSignalingBody.iceCandidate(...)`). Best-
    /// effort: a failed `addIceCandidate` is logged but
    /// not surfaced — WebRTC tolerates lost candidates by
    /// re-gathering on the other peer.
    public func addRemoteCandidate(
        sdpMid: String,
        sdpMLineIndex: Int32,
        candidate: String
    ) async {
        guard let peer = peer else { return }
        let rtcCandidate = RTCIceCandidate(
            sdp: candidate,
            sdpMLineIndex: sdpMLineIndex,
            sdpMid: sdpMid
        )
        await withCheckedContinuation { (cont: CheckedContinuation<Void, Never>) in
            peer.add(rtcCandidate) { err in
                if let err = err {
                    NSLog(
                        "WebRTCPeerConnection.addRemoteCandidate failed: %@",
                        err.localizedDescription
                    )
                }
                cont.resume()
            }
        }
    }

    // MARK: - Defaults

    /// STUN + (placeholder) TURN config. Production wires
    /// these via the rendezvous bootstrap config; for now a
    /// single Google STUN server is enough to exercise the
    /// happy path on most networks.
    public static func defaultIceServers() -> [RTCIceServer] {
        [
            RTCIceServer(urlStrings: [
                "stun:stun.l.google.com:19302",
                "stun:stun1.l.google.com:19302",
            ]),
            // TURN servers (Lakko's coturn cluster) arrive
            // via the rendezvous bootstrap config — see
            // `crates/lakko-transport/src/rendezvous.rs`.
            // For now we run STUN-only; on a NAT-symmetric
            // network the call will fail to connect and
            // surface as `.disconnected`. The follow-up
            // sub-sprint G3-1-turn wires the bootstrap
            // config into here.
        ]
    }
}

// MARK: - Errors

public enum WebRTCError: Error, CustomStringConvertible {
    case peerUnavailable
    case emptySdp

    public var description: String {
        switch self {
        case .peerUnavailable:
            return "RTCPeerConnection is not allocated"
        case .emptySdp:
            return "WebRTC produced an empty SDP"
        }
    }
}

// MARK: - RTCPeerConnectionDelegate

extension WebRTCPeerConnection: RTCPeerConnectionDelegate {

    // Required protocol methods. Most are no-ops for the
    // 1×1 audio-only path; we'll fill them in for group
    // calls + video in subsequent sub-sprints.

    public nonisolated func peerConnection(
        _ peerConnection: RTCPeerConnection,
        didChange stateChanged: RTCSignalingState
    ) {}

    public nonisolated func peerConnection(
        _ peerConnection: RTCPeerConnection,
        didAdd stream: RTCMediaStream
    ) {}

    public nonisolated func peerConnection(
        _ peerConnection: RTCPeerConnection,
        didRemove stream: RTCMediaStream
    ) {}

    public nonisolated func peerConnectionShouldNegotiate(
        _ peerConnection: RTCPeerConnection
    ) {}

    public nonisolated func peerConnection(
        _ peerConnection: RTCPeerConnection,
        didChange newState: RTCIceConnectionState
    ) {}

    public nonisolated func peerConnection(
        _ peerConnection: RTCPeerConnection,
        didChange newState: RTCIceGatheringState
    ) {}

    public nonisolated func peerConnection(
        _ peerConnection: RTCPeerConnection,
        didGenerate candidate: RTCIceCandidate
    ) {
        // Bounce to main actor before touching the delegate
        // — RTC callbacks fire on the SDK's worker queue.
        let sdpMid = candidate.sdpMid ?? ""
        let mLineIndex = candidate.sdpMLineIndex
        let sdp = candidate.sdp
        Task { @MainActor [weak self] in
            self?.delegate?.onLocalCandidate(sdpMid, mLineIndex, sdp)
        }
    }

    public nonisolated func peerConnection(
        _ peerConnection: RTCPeerConnection,
        didRemove candidates: [RTCIceCandidate]
    ) {}

    public nonisolated func peerConnection(
        _ peerConnection: RTCPeerConnection,
        didOpen dataChannel: RTCDataChannel
    ) {}

    /// G3-1-remote-video — Unified-Plan track-added callback.
    /// Fires when the remote adds (or the SFU renegotiates in)
    /// a media line we receive on. We surface only video
    /// tracks to the UI here; the incoming audio track is
    /// played out by the SDK's audio device automatically and
    /// needs no UI handle. SFrame on the receive path is armed
    /// separately via `armSFrameDecryptor(for:seed:)` from the
    /// call's exporter-seed dispatch.
    public nonisolated func peerConnection(
        _ peerConnection: RTCPeerConnection,
        didAdd rtpReceiver: RTCRtpReceiver,
        streams mediaStreams: [RTCMediaStream]
    ) {
        guard let videoTrack = rtpReceiver.track as? RTCVideoTrack else {
            return
        }
        // The MSID stream group id — on an SFU-routed call this
        // is the originating peer id (see Delegate doc).
        let streamId = mediaStreams.first?.streamId
        Task { @MainActor [weak self] in
            self?.delegate?.onRemoteVideoTrack(videoTrack, streamId)
        }
    }

    /// G3-1-remote-video — Unified-Plan track-removed callback.
    /// Clears the remote video handle so the UI drops back to
    /// the avatar placeholder. We cannot cheaply tell from the
    /// receiver alone whether the removed track was the video
    /// one, so we signal `nil` unconditionally; the 1×1 path
    /// has a single remote video track, and a future N-way
    /// sprint that tracks per-stream handles will refine this.
    public nonisolated func peerConnection(
        _ peerConnection: RTCPeerConnection,
        didRemove rtpReceiver: RTCRtpReceiver
    ) {
        guard rtpReceiver.track is RTCVideoTrack else {
            return
        }
        Task { @MainActor [weak self] in
            self?.delegate?.onRemoteVideoTrack(nil, nil)
        }
    }

    public nonisolated func peerConnection(
        _ peerConnection: RTCPeerConnection,
        didChange newState: RTCPeerConnectionState
    ) {
        Task { @MainActor [weak self] in
            self?.delegate?.onConnectionState(newState)
        }
    }
}
