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
        /// Emitted for EVERY freshly-added remote `RTCRtpReceiver`
        /// (audio AND video), unlike `onRemoteVideoTrack` which only
        /// surfaces video to the UI. The SFU session uses this to arm
        /// the SFrame decryptor on each incoming track via
        /// `armSFrameDecryptor(for:seed:)` — invariant #4 requires every
        /// remote track be decrypting, not just video. `streamId` is the
        /// MSID stream group = the source peer id on an SFU-routed call.
        /// Default no-op so the 1×1 direct path is unaffected.
        public let onRemoteReceiver: (RTCRtpReceiver, _ streamId: String?) -> Void
        public init(
            onLocalCandidate: @escaping (String, Int32, String) -> Void,
            onConnectionState: @escaping (RTCPeerConnectionState) -> Void,
            onRemoteVideoTrack: @escaping (RTCVideoTrack?, String?) -> Void = { _, _ in },
            onRemoteReceiver: @escaping (RTCRtpReceiver, String?) -> Void = { _, _ in }
        ) {
            self.onLocalCandidate = onLocalCandidate
            self.onConnectionState = onConnectionState
            self.onRemoteVideoTrack = onRemoteVideoTrack
            self.onRemoteReceiver = onRemoteReceiver
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
    /// `addLocalVideoTrack(_:)` on a video-enabled call.
    internal var localVideoSender: RTCRtpSender?

    // MARK: - Lifecycle

    /// Construct a peer connection.
    ///
    /// `iceServers` defaults to the STUN-only base list
    /// (`defaultIceServers()`); the per-call TURN row is
    /// appended by `CallSession.resolveIceServers()` before
    /// it constructs this peer (G3-1-turn). The caller can
    /// override for tests or for a region-pinned
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

    /// The underlying `RTCPeerConnection`'s current SDP
    /// signaling state (`nil` after `close()`). The SFU
    /// session's serialized mid-call renegotiation (piece B:
    /// `publishVideo`) only starts an offer while this is
    /// `.stable`, so its offer never overlaps an in-flight
    /// offer / answer.
    public var signalingState: RTCSignalingState? { peer?.signalingState }

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

    /// Toggle the local video track's `isEnabled` flag —
    /// the in-call "stop / start camera" control. Disabling
    /// halts frame delivery to the encoder (the remote sees
    /// the last frame freeze then a black frame); no SDP
    /// renegotiation, the m-line stays. No-op on an audio-only
    /// call (no local video track present).
    public func setCameraEnabled(_ on: Bool) {
        localVideoTrack?.isEnabled = on
    }

    /// G3-1-conn-quality — one-shot connection-stats sample
    /// read out of WebRTC's `RTCStatisticsReport`. Surfaces
    /// the active (nominated, succeeded) ICE candidate-pair's
    /// round-trip time + whether the path is relayed (TURN).
    /// `rttMs` is `nil` until WebRTC has a usable RTT estimate;
    /// `hasActivePair` is `false` before ICE settles.
    ///
    /// G3-1-conn-quality iter2 — `bytesReceived` / `bytesSent`
    /// are the candidate-pair's cumulative wire-byte counters
    /// (RTP + RTCP + STUN on the selected path). A single
    /// sample carries no rate; the caller diffs two samples
    /// against its own wall clock to derive a bitrate.
    public struct ConnectionStatsSnapshot: Sendable {
        public let rttMs: Int?
        public let relayed: Bool
        public let hasActivePair: Bool
        public let bytesReceived: Int64
        public let bytesSent: Int64
    }

    public func sampleConnectionStats() async -> ConnectionStatsSnapshot? {
        guard let peer = peer else { return nil }
        let report: RTCStatisticsReport = await withCheckedContinuation { cont in
            peer.statistics { cont.resume(returning: $0) }
        }
        let stats = report.statistics
        // Prefer the nominated succeeded pair; fall back to any
        // succeeded pair (some stacks don't flag `nominated`).
        let pairs = stats.values.filter { $0.type == "candidate-pair" }
        let active = pairs.first(where: {
            ($0.values["state"] as? String) == "succeeded"
                && (($0.values["nominated"] as? NSNumber)?.boolValue ?? false)
        }) ?? pairs.first(where: { ($0.values["state"] as? String) == "succeeded" })
        guard let pair = active else {
            return ConnectionStatsSnapshot(
                rttMs: nil, relayed: false, hasActivePair: false,
                bytesReceived: 0, bytesSent: 0
            )
        }
        let rttSec = (pair.values["currentRoundTripTime"] as? NSNumber)?.doubleValue
        let rttMs = rttSec.map { Int(($0 * 1000).rounded()) }
        let candType: (String?) -> String? = { id in
            guard let id = id, let s = stats[id] else { return nil }
            return s.values["candidateType"] as? String
        }
        let relayed = candType(pair.values["localCandidateId"] as? String) == "relay"
            || candType(pair.values["remoteCandidateId"] as? String) == "relay"
        let bytesReceived = (pair.values["bytesReceived"] as? NSNumber)?.int64Value ?? 0
        let bytesSent = (pair.values["bytesSent"] as? NSNumber)?.int64Value ?? 0
        return ConnectionStatsSnapshot(
            rttMs: rttMs, relayed: relayed, hasActivePair: true,
            bytesReceived: bytesReceived, bytesSent: bytesSent
        )
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
    /// On the initial-publish path this is called BEFORE the
    /// first SDP offer is built so that offer advertises a
    /// video m-line. It is ALSO valid mid-call (piece B:
    /// `SfuCallSession.publishVideo` adds a camera track to a
    /// call that started audio-only) — adding a track post-offer
    /// makes the offerer's next offer advertise the new m-line,
    /// which the SFU session then drives explicitly via
    /// `createRenegotiationOffer()` (not via
    /// `onnegotiationneeded`).
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

    /// Remove the local video track + its sender from the peer
    /// connection (mid-call add-video fail-closed cleanup, piece
    /// B2b). Called when the SFrame arm on a just-added video
    /// sender fails: the sender must not remain on the connection,
    /// so a subsequent renegotiation can never advertise an
    /// UNSEALED video m-line (invariant #4). Idempotent — a no-op
    /// when there is no video sender.
    public func removeLocalVideoTrack() {
        guard let peer = peer else { return }
        if let sender = localVideoSender {
            _ = peer.removeTrack(sender)
        }
        localVideoSender = nil
        localVideoTrack = nil
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
        // Each sender is bound to its own media-type key space
        // (ADR-0034): the audio sender seals on the audio space, the
        // video sender on the video space, so a frame counter shared
        // across the two streams of this leaf no longer reuses a nonce.
        let armSet: [(RTCRtpSender?, LakkoSFrameMediaType)] = [
            (localAudioSender, .audio),
            (localVideoSender, .video),
        ]
        for (maybeSender, mediaType) in armSet {
            guard let sender = maybeSender else { continue }
            let status = LakkoSFrameAttach.attachEncryptor(
                toSender: sender,
                seed: seed,
                localLeaf: localLeaf,
                mediaType: mediaType
            )
            if status != .OK {
                allOk = false
            }
        }
        return allOk
    }

    /// Arm SFrame on ONLY the local VIDEO sender (mid-call
    /// camera-on, piece B). The audio sender is SFrame-armed
    /// once at connect; re-running `armSFrameEncryptors` would
    /// attach a FRESH `LakkoFrameEncryptor` to it, resetting
    /// that sender's SFrame counter to 0 → nonce reuse under the
    /// same key. So the mid-call add-video path arms just the
    /// newly-added video sender and leaves the audio encryptor
    /// untouched.
    ///
    /// Returns `true` only when the video sender attached
    /// successfully; `false` when there is no video sender (a
    /// wiring bug — the caller must `addLocalVideoTrack(_:)`
    /// first) or the attach itself failed. A `false` return is
    /// fail-closed (invariant #4): the caller MUST NOT
    /// renegotiate an unsealed video m-line.
    public func armSFrameVideoEncryptor(seed: Data, localLeaf: UInt32) -> Bool {
        guard SFrameAvailability.shared.isCallingAvailable else {
            return false
        }
        guard let sender = localVideoSender else { return false }
        return LakkoSFrameAttach.attachEncryptor(
            toSender: sender,
            seed: seed,
            localLeaf: localLeaf,
            mediaType: .video
        ) == .OK
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
        // Bind the decryptor to the receive track's kind (ADR-0034) so
        // it derives each per-sender key on the same audio-or-video key
        // space the far-side encryptor sealed on. A wrong-space guess
        // fails closed (the frame will not open) rather than silently
        // opening on the wrong key — the audio track is the default only
        // because every receiver carries an audio or a video track.
        let mediaType: LakkoSFrameMediaType =
            (receiver.track is RTCVideoTrack) ? .video : .audio
        let status = LakkoSFrameAttach.attachDecryptor(
            toReceiver: receiver,
            seed: seed,
            mediaType: mediaType
        )
        return status == .OK
    }

    /// Default SFrame overlap window in ms (G-18, ADR-0029) — mirrors
    /// the web `SFRAME_OVERLAP_MS` + the Rust core `OVERLAP_DEFAULT_MS`.
    /// The decryptor keeps the previous epoch's key this long after a
    /// rekey so in-flight frames sealed under the old epoch still open.
    private static let sframeOverlapMs: UInt64 = 1000

    /// Mid-call rekey (G-18, ADR-0029) — rotate the SFrame key on every
    /// installed encryptor + decryptor to the new MLS epoch's `seed`,
    /// then close the overlap window after `sframeOverlapMs` by pruning
    /// the decryptors' retained previous key.
    ///
    /// Called by `SfuCallSession.rekey(newSeed:)` when a commit advances
    /// the group's epoch during a live call. Enumerates the live senders
    /// / receivers off the peer connection, so a receiver added by a
    /// later renegotiation is covered too; a sender / receiver that was
    /// never SFrame-armed is a benign skip.
    ///
    /// A rekey never downgrades to plaintext: if the rotation itself
    /// fails (bad seed), the bridge retains the current key (still
    /// E2EE, invariant #4). The `Bool` return is for observability only
    /// — `false` means at least one present transform's rotation failed;
    /// the caller keeps the call up.
    @discardableResult
    public func rekeySFrame(seed: Data) -> Bool {
        guard SFrameAvailability.shared.isCallingAvailable else {
            return false
        }
        guard let peer = peer else { return false }
        let now = UInt64(Date().timeIntervalSince1970 * 1000)
        let overlapMs = Self.sframeOverlapMs
        var allOk = true
        for sender in peer.senders {
            if !LakkoSFrameAttach.rekeyEncryptor(onSender: sender, seed: seed) {
                allOk = false
            }
        }
        for receiver in peer.receivers {
            if !LakkoSFrameAttach.rekeyDecryptor(
                onReceiver: receiver,
                seed: seed,
                nowMs: now,
                overlapMs: overlapMs
            ) {
                allOk = false
            }
        }
        // Close the overlap window: after `overlapMs`, drop the retained
        // previous key so seed-boundary forward secrecy takes full effect.
        // `Task {}` (not detached) inherits this @MainActor isolation, so
        // the prune runs back on the main actor.
        let overlapNs = overlapMs * 1_000_000
        Task { [weak self] in
            try? await Task.sleep(nanoseconds: overlapNs)
            self?.pruneSFrameDecryptors()
        }
        return allOk
    }

    /// Drop each decryptor's retained previous key once the overlap
    /// window has elapsed (G-18, ADR-0029). Re-enumerates the live
    /// receivers so the set reflects any mid-window renegotiation.
    private func pruneSFrameDecryptors() {
        guard let peer = peer else { return }
        let now = UInt64(Date().timeIntervalSince1970 * 1000)
        for receiver in peer.receivers {
            LakkoSFrameAttach.pruneDecryptor(onReceiver: receiver, nowMs: now)
        }
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

    /// Build a renegotiation SDP offer that reflects the CURRENT
    /// senders (mid-call add-track, piece B). Unlike
    /// `createOffer(audio:video:)` this does NOT force
    /// `OfferToReceiveAudio` / `OfferToReceiveVideo` — a mid-call
    /// offer just advertises whatever transceivers are now present
    /// (e.g. a video m-line added by `addLocalVideoTrack(_:)`
    /// after the initial publish). Sets the local description as a
    /// side effect. No rollback / glare handling here — the SFU
    /// session serializes so this only runs from a `.stable`
    /// state.
    public func createRenegotiationOffer() async throws -> String {
        guard let peer = peer else {
            throw WebRTCError.peerUnavailable
        }
        let constraints = RTCMediaConstraints(
            mandatoryConstraints: nil,
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

    /// Explicit SDP rollback for the 1×1 P2P perfect-negotiation
    /// glare path (piece B2b). Native WebRTC has NO implicit
    /// rollback on `setRemoteDescription` (unlike the browser,
    /// where a colliding renegotiation offer is rolled back
    /// transparently). The POLITE peer (the answerer) drives it
    /// explicitly: when a mid-call renegotiation offer arrives
    /// while it has an offer of its own in flight, it rolls its
    /// local offer back to `.stable`, then applies + answers the
    /// remote offer. The SFU mirror (`createRenegotiationOffer`)
    /// does NOT need this — it serialises offers so glare can't
    /// occur — but 1×1 glare is symmetric, so rollback is required.
    public func rollbackLocalDescription() async throws {
        guard let peer = peer else {
            throw WebRTCError.peerUnavailable
        }
        let rollback = RTCSessionDescription(type: .rollback, sdp: "")
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            peer.setLocalDescription(rollback) { err in
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

    /// STUN-only base ICE-server list. This is the floor every
    /// call starts from; TURN is layered on top per-call by
    /// `CallSession.resolveIceServers()`, which appends one
    /// freshly-minted coturn row via `TurnCredentialClient`
    /// (G3-1-turn, landed). Keeping the default STUN-only here
    /// means a call with no `turnClient` injected still attempts
    /// to connect over STUN (only symmetric-NAT peers fail, as
    /// `.disconnected`).
    public static func defaultIceServers() -> [RTCIceServer] {
        [
            RTCIceServer(urlStrings: [
                "stun:stun.l.google.com:19302",
                "stun:stun1.l.google.com:19302",
            ]),
            // TURN rows (Lakko's coturn cluster) are NOT added
            // here — `CallSession.resolveIceServers()` mints one
            // at call start via `TurnCredentialClient.fetchOrNil()`
            // and appends it to this base list. See
            // `services/auth/src/turn.rs` for the credential REST
            // shape.
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
        // The MSID stream group id — on an SFU-routed call this
        // is the originating peer id (see Delegate doc).
        let streamId = mediaStreams.first?.streamId
        // Surface EVERY receiver (audio + video) so the SFU session can
        // arm the SFrame decryptor on each incoming track (invariant #4).
        Task { @MainActor [weak self] in
            self?.delegate?.onRemoteReceiver(rtpReceiver, streamId)
        }
        // Video tracks additionally surface to the UI for rendering;
        // audio plays out through the SDK's audio device automatically.
        guard let videoTrack = rtpReceiver.track as? RTCVideoTrack else {
            return
        }
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
