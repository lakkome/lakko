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

    /// The queue every blocking libwebrtc entry point is called on.
    ///
    /// `RTCPeerConnection`'s `offer` / `answer` / `setLocalDescription` /
    /// `setRemoteDescription` / `add(candidate)` / `close` look asynchronous
    /// (they take completion handlers) but each one first proxies to
    /// libwebrtc's signaling thread and BLOCKS the caller until that thread
    /// has done the work. Applying the first answer starts audio, which on
    /// the worker thread initialises the voice-processing audio unit, and on
    /// the simulator that unit's RPC to the audio server needs the app's main
    /// run loop to be alive. Called from the main actor, the wait deadlocked:
    /// the main thread slept in `setRemoteDescription`, the audio server never
    /// got its answer, and CoreAudio aborted the process ten seconds later
    /// (`_ReportRPCTimeout` under `AudioUnitInitialize`, three crash reports
    /// in one hour). Serial, so the calls keep the order they were issued in.
    private static let libwebrtcQueue = DispatchQueue(
        label: "me.lakko.webrtc.blocking", qos: .userInitiated)


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
        // Before ANY other call into WebRTC, per `RTCFieldTrials.h`. This is the
        // one initialiser every peer connection must pass through, so a trial
        // installed here cannot be missed by a code path that skipped app
        // launch — and without it a frame-encrypted VP8 stream never assembles
        // a single frame at the receiver (see `WebRTCFieldTrials`).
        WebRTCFieldTrials.installOnce()
        WebRTCDiagnosticLogging.installOnce()
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

    /// This device's own SDP, for diagnostics that compare what was announced
    /// against what is actually on the wire.
    public var localDescriptionSdp: String { peer?.localDescription?.sdp ?? "" }

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
        // Every sender must carry a frame encryptor and every receiver a frame
        // decryptor, or no media flows. This is our stated posture already
        // (invariant #4, no fallback to non-E2EE) and it was not being declared
        // — which is what silently broke video.
        //
        // `rtp_video_stream_receiver2.cc:330` builds the video decryption stage
        // ONLY under this flag:
        //
        //     // Only construct the encrypted receiver if frame encryption is enabled.
        //     if (config_.crypto_options.sframe.require_frame_encryption) {
        //       buffered_frame_decryptor_ = std::make_unique<BufferedFrameDecryptor>(...);
        //       if (frame_decryptor != nullptr) { ...SetFrameDecryptor... }
        //     }
        //
        // With the flag off, a receive stream built from a config that ALREADY
        // carries our decryptor discards it — the `frame_decryptor != nullptr`
        // arm is nested inside the disabled branch. Frames then run to the
        // decoder as SFrame ciphertext and every one fails to decode, while our
        // `Decrypt` is never called: attached, never consulted.
        //
        // Audio never had this. `ChannelReceive` holds its decryptor as a plain
        // member and consults it inline on the receive path
        // (`audio/channel_receive.cc:689`), so there is no construction-time
        // flag that can take its decryption stage away. That asymmetry is the
        // whole reason audio worked and video did not.
        let crypto = RTCCryptoOptions(
            srtpEnableGcmCryptoSuites: false,
            srtpEnableAes128Sha1_32CryptoCipher: false,
            srtpEnableEncryptedRtpHeaderExtensions: false,
            sframeRequireFrameEncryption: true)
        config.cryptoOptions = crypto

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
        // Off the main thread for the reason `libwebrtcQueue` gives: `close`
        // blocks on the signaling thread, which tears the audio unit down.
        Self.libwebrtcQueue.async { p.close() }
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
        /// Inbound RTP packets, summed across every receiver.
        ///
        /// Separate from `bytesReceived` on purpose: that one is the candidate
        /// pair's, which counts ICE consent checks, so it keeps climbing for
        /// as long as the peer's ICE agent is alive whether or not there is a
        /// call left. Only this one moves when media actually arrives, which
        /// is what `MediaWatchdog` needs.
        public let mediaPacketsReceived: Int64
        /// RTP packets this device has SENT. Paired with
        /// ``mediaPacketsReceived`` so one sample can say which DIRECTION of a
        /// silent call is dead: a call with `in == 0` and `out > 0` is not the
        /// same defect as one with both at zero, and from one end alone the
        /// two are indistinguishable.
        public let mediaPacketsSent: Int64
        /// The SSRC this device is sending on (0 when nothing is sent yet).
        public let mediaSsrcSent: Int64
    }

    public func sampleConnectionStats() async -> ConnectionStatsSnapshot? {
        guard let peer = peer else { return nil }
        let report: RTCStatisticsReport = await withCheckedContinuation { cont in
            peer.statistics { cont.resume(returning: $0) }
        }
        let stats = report.statistics
        let mediaPackets = stats.values
            .filter { $0.type == "inbound-rtp" }
            .reduce(Int64(0)) { sum, s in
                sum + ((s.values["packetsReceived"] as? NSNumber)?.int64Value ?? 0)
            }
        let sentPackets = stats.values
            .filter { $0.type == "outbound-rtp" }
            .reduce(Int64(0)) { sum, s in
                sum + ((s.values["packetsSent"] as? NSNumber)?.int64Value ?? 0)
            }
        // The SSRC actually being sent on. The peer can only demux what its
        // own answer announced, so a value here that differs from the
        // signalled one is a call whose media is dropped on arrival.
        let sentSsrc = stats.values
            .first(where: { $0.type == "outbound-rtp" })
            .flatMap { ($0.values["ssrc"] as? NSNumber)?.int64Value } ?? 0
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
                bytesReceived: 0, bytesSent: 0, mediaPacketsReceived: mediaPackets,
                mediaPacketsSent: sentPackets, mediaSsrcSent: sentSsrc
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
            bytesReceived: bytesReceived, bytesSent: bytesSent,
            mediaPacketsReceived: mediaPackets,
            mediaPacketsSent: sentPackets,
            mediaSsrcSent: sentSsrc
        )
    }

    /// Read the `inbound-rtp` counters for the remote VIDEO stream.
    ///
    /// Separates the three reasons a remote picture can be missing — nothing
    /// arriving, arriving but not decoding, decoding but not drawn — which the
    /// screen cannot tell apart and which have opposite fixes. See
    /// ``RemoteVideoDiagnosis``.
    ///
    /// Returns ``InboundVideoStats/none`` when the report carries no inbound
    /// video stream at all, which is itself the answer when a track has
    /// supposedly attached.
    public func sampleInboundVideoStats() async -> InboundVideoStats {
        guard let peer = peer else { return .none }
        let report: RTCStatisticsReport = await withCheckedContinuation { cont in
            peer.statistics { cont.resume(returning: $0) }
        }
        // `kind` is the current spelling; `mediaType` is the older one and some
        // builds still emit only that, so both are accepted rather than
        // silently reporting "no video stream" on a naming difference.
        let inbound = report.statistics.values.first { stat in
            guard stat.type == "inbound-rtp" else { return false }
            let kind = (stat.values["kind"] as? String)
                ?? (stat.values["mediaType"] as? String)
            return kind == "video"
        }
        guard let inbound else { return .none }
        let number: (String) -> Int64 = { key in
            (inbound.values[key] as? NSNumber)?.int64Value ?? 0
        }
        // The negotiated codec, resolved through the stream's `codecId`. It is
        // what separates a frame the decryptor rejected from a frame this device
        // cannot decode — both count packets and decode nothing.
        var codec: String?
        if let codecId = inbound.values["codecId"] as? String,
            let codecStat = report.statistics[codecId] {
            codec = codecStat.values["mimeType"] as? String
        }
        return InboundVideoStats(
            packetsReceived: number("packetsReceived"),
            framesReceived: number("framesReceived"),
            framesDecoded: number("framesDecoded"),
            framesDropped: number("framesDropped"),
            bytesReceived: number("bytesReceived"),
            pliCount: number("pliCount"),
            keyFramesDecoded: number("keyFramesDecoded"),
            codec: codec,
            decoder: inbound.values["decoderImplementation"] as? String)
    }

    /// The audio entries of the statistics report, for the sound levels a
    /// group call reads (`docs/ux/group-call-grid.md` section 7.3): the
    /// `inbound-rtp` audio streams and the local `media-source`. `nil` when
    /// there is no peer connection.
    ///
    /// Only the entries: which person a stream belongs to, and what a level
    /// means, is ``CallAudioLevelStats``'s, so it can be tested without a
    /// peer connection.
    func sampleAudioStatEntries() async -> [CallStatEntry]? {
        guard let peer = peer else { return nil }
        let report: RTCStatisticsReport = await withCheckedContinuation { cont in
            peer.statistics { cont.resume(returning: $0) }
        }
        return report.statistics.values
            .filter { $0.type == "inbound-rtp" || $0.type == "media-source" }
            .map { CallStatEntry(type: $0.type, values: $0.values) }
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

    /// A video track backed by `source` that belongs to no peer connection.
    ///
    /// `docs/ux/calls.md` section 3.2 — the camera check happens BEFORE the call
    /// is placed, so there is no peer connection to hang a track on yet, and
    /// building one early would mean an offer for a call the user has not asked
    /// for. The check renders this track; the dial that follows builds its own
    /// through ``addLocalVideoTrack(_:)``.
    public static func newPreviewVideoTrack(source: RTCVideoSource) -> RTCVideoTrack {
        sharedFactory.videoTrack(with: source, trackId: "lakko-preview0")
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

    /// Stop or resume OFFERING video, without touching the sender.
    ///
    /// `docs/ux/calls.md` section 4.1 — "turning the camera off again returns
    /// the call to the portrait", on BOTH screens. Disabling the track alone
    /// does not do that: libwebrtc keeps transmitting (black frames), nothing
    /// fires on the peer, and their screen stays on a full-screen still. What
    /// the peer does react to is the transceiver's direction, so this flips it
    /// between `sendRecv` and `recvOnly` and leaves the renegotiation to the
    /// caller.
    ///
    /// **The sender is deliberately kept.** Removing the track would drop its
    /// SFrame encryptor, and the next camera-on would arm a fresh one for the
    /// same `(leaf, stream)` — restarting the frame counter at zero and reusing
    /// `(key, nonce)` pairs already spent, which is exactly the reuse ADR-0035
    /// exists to prevent. Keeping the sender keeps the encryptor, and with it
    /// the counter.
    ///
    /// - Returns: `true` when the direction actually changed, so the caller
    ///   knows a renegotiation is owed. `false` (no video sender, no matching
    ///   transceiver, already in that direction, or the platform refused) leaves
    ///   the connection exactly as it was.
    public func setVideoSendingEnabled(_ sending: Bool) -> Bool {
        guard let peer = peer, let sender = localVideoSender else { return false }
        guard let transceiver = peer.transceivers.first(where: { $0.sender == sender })
        else { return false }
        let wanted: RTCRtpTransceiverDirection = sending ? .sendRecv : .recvOnly
        guard transceiver.direction != wanted else { return false }
        var error: NSError?
        transceiver.setDirection(wanted, error: &error)
        if let error {
            NSLog(
                "WebRTCPeerConnection: video direction change refused: %@",
                String(describing: error))
            return false
        }
        return true
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
        // Each sender is bound to its own per-sender stream ordinal
        // (ADR-0035): the audio sender seals under stream 0, the video
        // sender under stream 1. The seal KID is
        // `(localLeaf << 8) | streamIndex`, so a frame counter shared
        // across the two streams of this leaf keys into disjoint
        // `(key, nonce_salt)` spaces and no longer reuses a nonce.
        let armSet: [(RTCRtpSender?, UInt8)] = [
            (localAudioSender, 0),
            (localVideoSender, 1),
        ]
        for (maybeSender, streamIndex) in armSet {
            guard let sender = maybeSender else { continue }
            let status = LakkoSFrameAttach.attachEncryptor(
                toSender: sender,
                seed: seed,
                localLeaf: localLeaf,
                streamIndex: streamIndex
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
        // Stream 1 (ADR-0035): v1 carries at most one video track; a
        // future second video / screen-share would use stream 2.
        return LakkoSFrameAttach.attachEncryptor(
            toSender: sender,
            seed: seed,
            localLeaf: localLeaf,
            streamIndex: 1
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
        // No media/stream argument (ADR-0035): a single decryptor bound
        // to this remote receive track opens every frame on it by
        // reading the composite KID (`(senderLeaf << 8) | streamIndex`)
        // from each frame's own SFrame header, so audio and either video
        // stream are each keyed correctly with no local track-kind signal.
        let status = LakkoSFrameAttach.attachDecryptor(
            toReceiver: receiver,
            seed: seed
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
            Self.libwebrtcQueue.async {
            peer.offer(for: constraints) { sdp, err in
                if let err = err { cont.resume(throwing: err); return }
                guard let sdp = sdp else {
                    cont.resume(throwing: WebRTCError.emptySdp); return
                }
                cont.resume(returning: sdp)
            }
            }
        }
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            Self.libwebrtcQueue.async {
            peer.setLocalDescription(offer) { err in
                if let err = err {
                    cont.resume(throwing: err)
                } else {
                    cont.resume()
                }
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
            Self.libwebrtcQueue.async {
            peer.offer(for: constraints) { sdp, err in
                if let err = err { cont.resume(throwing: err); return }
                guard let sdp = sdp else {
                    cont.resume(throwing: WebRTCError.emptySdp); return
                }
                cont.resume(returning: sdp)
            }
            }
        }
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            Self.libwebrtcQueue.async {
            peer.setLocalDescription(offer) { err in
                if let err = err {
                    cont.resume(throwing: err)
                } else {
                    cont.resume()
                }
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
            Self.libwebrtcQueue.async {
            peer.setRemoteDescription(remoteOffer) { err in
                if let err = err {
                    cont.resume(throwing: err)
                } else {
                    cont.resume()
                }
            }
            }
        }
        let answer: RTCSessionDescription = try await withCheckedThrowingContinuation { cont in
            Self.libwebrtcQueue.async {
            peer.answer(for: RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil)) { sdp, err in
                if let err = err { cont.resume(throwing: err); return }
                guard let sdp = sdp else {
                    cont.resume(throwing: WebRTCError.emptySdp); return
                }
                cont.resume(returning: sdp)
            }
            }
        }
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            Self.libwebrtcQueue.async {
            peer.setLocalDescription(answer) { err in
                if let err = err {
                    cont.resume(throwing: err)
                } else {
                    cont.resume()
                }
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
            Self.libwebrtcQueue.async {
            peer.setRemoteDescription(remoteAnswer) { err in
                if let err = err {
                    cont.resume(throwing: err)
                } else {
                    cont.resume()
                }
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
            Self.libwebrtcQueue.async {
            peer.setLocalDescription(rollback) { err in
                if let err = err {
                    cont.resume(throwing: err)
                } else {
                    cont.resume()
                }
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
            Self.libwebrtcQueue.async {
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
