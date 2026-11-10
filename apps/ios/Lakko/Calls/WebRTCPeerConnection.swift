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
        /// The id of a remote video track that was removed. Fired just before
        /// the `onRemoteVideoTrack(nil, nil)` of the same removal, so a caller
        /// holding more than one remote video track (a camera and a shared
        /// screen, ADR-0083) can tell which one went. Default no-op.
        public let onRemoteVideoTrackRemoved: (_ trackId: String) -> Void
        public init(
            onLocalCandidate: @escaping (String, Int32, String) -> Void,
            onConnectionState: @escaping (RTCPeerConnectionState) -> Void,
            onRemoteVideoTrack: @escaping (RTCVideoTrack?, String?) -> Void = { _, _ in },
            onRemoteReceiver: @escaping (RTCRtpReceiver, String?) -> Void = { _, _ in },
            onRemoteVideoTrackRemoved: @escaping (String) -> Void = { _ in }
        ) {
            self.onLocalCandidate = onLocalCandidate
            self.onConnectionState = onConnectionState
            self.onRemoteVideoTrack = onRemoteVideoTrack
            self.onRemoteReceiver = onRemoteReceiver
            self.onRemoteVideoTrackRemoved = onRemoteVideoTrackRemoved
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
        // ADR-0088: the default audio processing module with Lakko's noise
        // suppressor as its capture post-processing stage. Voice processing
        // is NOT bypassed: the voice-processing I/O unit (the platform's echo
        // canceller) still runs, ahead of the suppressor. With `false` this
        // initialiser builds the same audio device module as the plain one.
        return RTCPeerConnectionFactory(
            bypassVoiceProcessing: false,
            encoderFactory: encoderFactory,
            decoderFactory: decoderFactory,
            audioProcessingModule: CaptureNoiseSuppression.audioProcessingModule()
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

    /// Logged once per peer connection; see `preferVp8(on:)`.
    private var vp8Warned = false

    // MARK: - Lifecycle

    /// Construct a peer connection.
    ///
    /// `iceServers` is the call's list, built by
    /// ``IceServers`` (Lakko's own servers only: the minted
    /// TURN row and STUN on its host, or none) in
    /// `CallSession` / `SfuCallSession` before they
    /// construct this peer. It defaults to empty — host
    /// candidates only, never a public STUN server.
    public init(iceServers: [RTCIceServer] = []) {
        super.init()
        let servers = iceServers

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
        // ADR-0088: this call's noise suppressor goes with it (a no-op when
        // another call has armed since).
        CaptureNoiseSuppression.processor.disarm(forOwner: self)
        // Off the main thread for the reason `libwebrtcQueue` gives: `close`
        // blocks on the signaling thread, which tears the audio unit down.
        Self.libwebrtcQueue.async { p.close() }
        peer = nil
        localAudioTrack = nil
        if localVideoTrack != nil { CameraBackgroundStage.shared?.setCameraSending(false) }
        localVideoTrack = nil
        cameraCopies = [:]
        sframeSenders = []
        sframeReceivers = []
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
        // ADR-0088: this call's microphone gets a fresh noise suppressor when
        // the switch is on; the line it logs says which processing applies.
        CaptureNoiseSuppression.processor.arm(
            forOwner: self,
            on: NoiseSuppressionSetting.shared.currentChoice,
            platform: CaptureNoiseSuppression.platformEffects)
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
        // The copies of an SFU call's camera carry the same picture and are
        // switched with it.
        for copy in cameraCopies.values { copy.track.isEnabled = on }
        // ADR-0089: the camera background runs only while the camera sends.
        if localVideoTrack != nil { CameraBackgroundStage.shared?.setCameraSending(on) }
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

    /// The `inbound-rtp` entries of the statistics report, audio and video,
    /// for the sound-and-picture alignment of a group call
    /// (``AvAlignStats``): one read of the whole connection's report per
    /// reading interval. `nil` when there is no peer connection.
    func sampleInboundStatEntries() async -> [CallStatEntry]? {
        guard let peer = peer else { return nil }
        let report: RTCStatisticsReport = await withCheckedContinuation { cont in
            peer.statistics { cont.resume(returning: $0) }
        }
        return report.statistics.values
            .filter { $0.type == "inbound-rtp" }
            .map { CallStatEntry(type: $0.type, values: $0.values) }
    }

    /// Set the minimum playout delay of a remote `receiver` to `ms`
    /// milliseconds (`0` gives it back to the engine), for the
    /// sound-and-picture alignment of a group call (``AvReceiverDelay``).
    ///
    /// The engine's receiver is a proxy that blocks the caller until the
    /// worker thread has run the call, so the set runs on the queue every
    /// other blocking libwebrtc call uses and never on the main thread
    /// (``libwebrtcQueue``). Serial, so sets keep the order they were asked
    /// in. The receiver is retained for the call.
    func setReceiverPlayoutDelay(
        _ receiver: RTCRtpReceiver, ms: UInt32
    ) async -> LakkoReceiverDelayStatus {
        await withCheckedContinuation { cont in
            Self.libwebrtcQueue.async {
                cont.resume(
                    returning: LakkoReceiverDelay.setMinimumPlayoutDelayMs(
                        ms, forReceiver: receiver))
            }
        }
    }

    /// The media entries of the statistics report (`inbound-rtp`,
    /// `outbound-rtp` and the `media-source` each sender reads from, audio
    /// and video), with their ids, for the calls' media log
    /// (``CallMediaCounters``). `nil` when there is no peer connection.
    func sampleMediaStatEntries() async -> [CallMediaStatEntry]? {
        guard let peer = peer else { return nil }
        let report: RTCStatisticsReport = await withCheckedContinuation { cont in
            peer.statistics { cont.resume(returning: $0) }
        }
        return report.statistics
            .filter {
                $0.value.type == "inbound-rtp" || $0.value.type == "outbound-rtp"
                    || $0.value.type == "media-source"
            }
            .map { CallMediaStatEntry(id: $0.key, type: $0.value.type, values: $0.value.values) }
    }

    /// The ICE entries of the statistics report (`transport`,
    /// `candidate-pair`, `local-candidate`, `remote-candidate`), with their
    /// ids, for the call's path line (``CallPath``). `nil` when there is no
    /// peer connection.
    func samplePathStatEntries() async -> [CallMediaStatEntry]? {
        guard let peer = peer else { return nil }
        let report: RTCStatisticsReport = await withCheckedContinuation { cont in
            peer.statistics { cont.resume(returning: $0) }
        }
        return report.statistics
            .filter { CallPath.entryTypes.contains($0.value.type) }
            .map { CallMediaStatEntry(id: $0.key, type: $0.value.type, values: $0.value.values) }
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
        // ADR-0089: a new camera track sends; the background runs for it.
        CameraBackgroundStage.shared?.setCameraSending(true)
    }

    // MARK: - The SFU call's camera: three senders (ADR-0070 steps 1 and 5)

    /// A copy of the camera beside the main sender: a further track on a
    /// source of its own fed the camera's frames (``CapturerFan``), on a
    /// transceiver of its own, scaled down by its one encoding
    /// (``CameraSender``).
    struct CameraCopy {
        let track: RTCVideoTrack
        let transceiver: RTCRtpTransceiver
        var sender: RTCRtpSender { transceiver.sender }
    }

    /// The copies this connection sends, by kind. Never more than one of a
    /// kind; empty on the one-to-one path.
    private var cameraCopies: [CameraCopyKind: CameraCopy] = [:]

    /// The exact sender and receiver objects an SFrame transform was attached
    /// to (the bridge keeps it as an associated object on that object). A
    /// rekey and a prune walk these, never `peer.senders` / `peer.receivers`:
    /// those build new wrapper objects on every read, which carry no
    /// association, so a rekey over them was a silent no-op. Found on an
    /// iPhone on 2026-11-10: in a call link the owner's senders kept sealing
    /// under the seed of before the web joiner's admission and the web opened
    /// none of its frames, while the owner, whose decryptor for the joiner was
    /// armed after the rekey, opened the joiner's.
    private var sframeSenders: [RTCRtpSender] = []
    private var sframeReceivers: [RTCRtpReceiver] = []

    private func rememberSFrame(sender: RTCRtpSender) {
        if !sframeSenders.contains(where: { $0 === sender }) { sframeSenders.append(sender) }
    }

    /// Add the camera of an SFU call (group and link calls) as its main
    /// sender: the picture as captured, one encoding, SFrame stream index 1
    /// as before. Created with `add(_:streamIds:)`, the way the one-to-one
    /// call creates its sender, on the first publish and on a mid-call
    /// camera-on alike (mid-call it may reuse a video transceiver the SFU
    /// already negotiated). The copies are added by ``addCameraCopies(sources:middleActive:)``.
    func addLocalCameraTrack(_ source: RTCVideoSource) {
        guard let peer = peer else { return }
        let track = Self.sharedFactory.videoTrack(
            with: source, trackId: CameraSender.trackId(of: .main))
        // On a transceiver of its own, never `add(_:streamIds:)`: mid-call
        // that hands the camera the first video transceiver whose sender
        // never sent, which is one the SFU opened to bring this device
        // another person's camera, and the SFU does not take what arrives on
        // its own downstream line as this person's main picture (the web
        // client did this until 2026-10-23: its camera turned on after an
        // Android one reached the others only as the light and middle
        // copies). Android adds its group camera the same way.
        let transceiverInit = RTCRtpTransceiverInit()
        transceiverInit.direction = .sendRecv
        transceiverInit.streamIds = ["lakko-local-stream"]
        let sender = peer.addTransceiver(with: track, init: transceiverInit)?.sender
        self.localVideoTrack = track
        self.localVideoSender = sender
        if let sender { configureMainCameraSender(sender) }
        // ADR-0089: a new camera track sends; the background runs for it.
        CameraBackgroundStage.shared?.setCameraSending(true)
    }

    /// Ask the main camera sender for its temporal layers
    /// (``CameraSender/mainTemporalLayers``) and for the frame-rate
    /// preference, in one set of its parameters, then read the layers back
    /// and say in one line what the stack kept. The transceiver is added
    /// without encodings, so this is the only way to ask. Best effort: a stack that
    /// refuses keeps one layer, and the line says so.
    private func configureMainCameraSender(_ sender: RTCRtpSender) {
        let asked = CameraSender.mainTemporalLayers
        let framerate = NSNumber(value: RTCDegradationPreference.maintainFramerate.rawValue)
        SenderParameterWrites.enqueue(sender, on: Self.libwebrtcQueue) { parameters, set in
            if let first = parameters.encodings.first {
                first.numTemporalLayers = NSNumber(value: asked)
            }
            parameters.degradationPreference = framerate
            let kept = set(parameters).encodings.first?.numTemporalLayers?.intValue
            NSLog(
                "WebRTCPeerConnection: %@",
                CameraSender.temporalLayersLine(asked: asked, kept: kept))
        }
    }

    /// Add the light and the middle copy of the camera, each an ordinary
    /// sender on a transceiver of its own, in that order (so the light one
    /// keeps the media line it had before there was a middle one). Call it
    /// right after ``addLocalCameraTrack(_:)``, before SFrame is armed and
    /// before any offer is built.
    ///
    /// `sources` gives each copy its own source, never the main sender's:
    /// one capturer feeds all three through a ``CapturerFan``
    /// (``LocalCameraCapture/copySource(_:)``). On one shared source the
    /// smallest picture any of the three encoders asked for was served to
    /// all of them, so the adaptation at the low start bitrate held the main
    /// and the middle picture down for the whole call. The copy's size is
    /// its encoding's `scaleResolutionDownBy` of its own source.
    /// `middleActive` says whether the middle sender is created sending
    /// (``SenderActivityRule``); the light one always is. A copy with no
    /// source is absent, said once.
    ///
    /// Never fails the call: a copy the binding will not take is absent,
    /// said once, and the call goes on without it. A copy that exists is
    /// left alone.
    ///
    /// A transceiver refused with its encoding is asked for again without
    /// one, and the size, the ceiling and the activity are then set through
    /// the sender's parameters (``CameraSender/copyAddStep(withEncodingTaken:plainTaken:)``,
    /// the web client's fallback). A copy that cannot be sized that way is
    /// removed here, before SFrame is armed and before any offer: the camera
    /// is never sent another time at full size.
    func addCameraCopies(
        sources: (CameraCopyKind) -> RTCVideoSource?, middleActive: Bool
    ) {
        guard let peer = peer, localVideoSender != nil else { return }
        let plan = CameraSender.plan(middleActive: middleActive)
        for kind in CameraCopyKind.addOrder where cameraCopies[kind] == nil {
            guard let spec = plan.first(where: { $0.kind == kind.senderKind }) else { continue }
            guard let source = sources(kind) else {
                NSLog(
                    "WebRTCPeerConnection: publishing the camera without a %@ sender: "
                        + "no video source for it", kind.rawValue)
                continue
            }
            let track = Self.sharedFactory.videoTrack(with: source, trackId: spec.trackId)
            track.isEnabled = localVideoTrack?.isEnabled ?? true
            let transceiverInit = RTCRtpTransceiverInit()
            transceiverInit.direction = .sendOnly
            transceiverInit.sendEncodings = [CameraSender.rtcEncoding(spec.encoding)]
            var transceiver = peer.addTransceiver(with: track, init: transceiverInit)
            let step = CameraSender.copyAddStep(
                withEncodingTaken: transceiver != nil,
                plainTaken: {
                    let plainInit = RTCRtpTransceiverInit()
                    plainInit.direction = .sendOnly
                    transceiver = peer.addTransceiver(with: track, init: plainInit)
                    return transceiver != nil
                }())
            guard let transceiver, step != .absent else {
                track.isEnabled = false
                NSLog(
                    "WebRTCPeerConnection: publishing the camera without a %@ sender: "
                        + "the transceiver was refused", kind.rawValue)
                continue
            }
            cameraCopies[kind] = CameraCopy(track: track, transceiver: transceiver)
            if step == .configureWithParameters {
                NSLog(
                    "WebRTCPeerConnection: the %@ camera transceiver was refused with its "
                        + "encoding, added without and sized through the sender", kind.rawValue)
                guard configureCameraCopy(transceiver.sender, wanted: spec.encoding) else {
                    removeCameraCopy(kind, reason: "its size and bitrate ceiling were refused")
                    continue
                }
            }
            preferFramerate(transceiver.sender)
        }
    }

    /// Give an already-added copy's sender its size, its bitrate ceiling and
    /// its activity through the sender's parameters, and read them back: the
    /// binding's setter reports nothing, so what the sender holds afterwards
    /// is the only answer. `false` when the sender has no encoding or does
    /// not hold what was asked.
    ///
    /// Made on the calling thread, the one parameter write that does not go
    /// through ``SenderParameterWrites``: the caller must know the answer
    /// before SFrame is armed and an offer is built, and waiting for the
    /// queue from the main thread is the deadlock ``libwebrtcQueue`` exists
    /// to avoid. It cannot overlap another write of the same sender: the
    /// sender was created earlier in this same main-actor turn, and every
    /// other writer (its frame-rate preference, its activity, its ceiling) is
    /// asked from the main actor after this returns.
    private func configureCameraCopy(_ sender: RTCRtpSender, wanted: CameraEncodingSpec) -> Bool {
        let parameters = sender.parameters
        guard let first = parameters.encodings.first else { return false }
        CameraSender.apply(wanted, to: first)
        sender.parameters = parameters
        return CameraSender.copyIsSized(
            wanted: wanted, readBack: sender.parameters.encodings.first.map(CameraSender.spec(of:)))
    }

    /// Take a copy's sender off the connection. Used when it could not be
    /// sealed: an unsealed sender is never left in place (invariant #4).
    func removeCameraCopy(_ kind: CameraCopyKind, reason: String) {
        guard let copy = cameraCopies.removeValue(forKey: kind) else { return }
        NSLog(
            "WebRTCPeerConnection: publishing the camera without a %@ sender: %@",
            kind.rawValue, reason)
        copy.track.isEnabled = false
        LakkoSFrameAttach.detach(fromSender: copy.sender)
        sframeSenders.removeAll { $0 === copy.sender }
        if let peer = peer {
            _ = peer.removeTrack(copy.sender)
        }
        copy.transceiver.stopInternal()
    }

    /// One of the camera's senders, or `nil` when this connection has none
    /// of that kind.
    func cameraSender(_ kind: CameraSenderKind) -> RTCRtpSender? {
        switch kind {
        case .main: return localVideoSender
        case .light: return cameraCopies[.light]?.sender
        case .middle: return cameraCopies[.middle]?.sender
        }
    }

    /// The `mid`s of the offer's m-lines that carry the `kind` copy of the
    /// camera: one once a local description that includes the copy is set,
    /// none otherwise (``CameraSender/mids(ofCopyMid:)``). Sent with every
    /// offer this client makes.
    func cameraCopyMids(_ kind: CameraCopyKind) -> [String] {
        CameraSender.mids(ofCopyMid: cameraCopies[kind]?.transceiver.mid)
    }

    /// Which camera sender each of this connection's media lines carries,
    /// by `mid`, for the media log. A sender whose transceiver has no `mid`
    /// yet is absent.
    func cameraSenderMids() -> [String: CameraSenderKind] {
        var out: [String: CameraSenderKind] = [:]
        for kind in CameraCopyKind.addOrder {
            for mid in cameraCopyMids(kind) { out[mid] = kind.senderKind }
        }
        if let peer = peer, let main = localVideoSender,
            let mid = peer.transceivers.first(where: { $0.sender == main })?.mid, !mid.isEmpty {
            out[mid] = .main
        }
        return out
    }

    /// A camera sender as ``SenderActivity`` drives it: its first encoding's
    /// `isActive`, read and set off the main thread. `nil` without that
    /// sender. The same object for the same sender, so a re-bind of an
    /// unchanged sender is recognised as such.
    func cameraActivitySender(_ kind: CameraSenderKind) -> RtcActivitySender? {
        guard let sender = cameraSender(kind) else {
            activitySenders[kind] = nil
            return nil
        }
        if let held = activitySenders[kind], held.sender === sender { return held }
        let made = RtcActivitySender(sender: sender, queue: Self.libwebrtcQueue)
        activitySenders[kind] = made
        return made
    }

    private var activitySenders: [CameraSenderKind: RtcActivitySender] = [:]

    /// A camera sender as ``SenderCeiling`` drives it: its first encoding's
    /// `maxBitrateBps`, read and set off the main thread. `nil` without that
    /// sender. The same object for the same sender, so a re-bind of an
    /// unchanged sender is recognised as such, and a new object for a sender
    /// created again, which then gets the ceiling in force.
    func cameraCeilingSender(_ kind: CameraSenderKind) -> RtcCeilingSender? {
        guard let sender = cameraSender(kind) else {
            ceilingSenders[kind] = nil
            return nil
        }
        if let held = ceilingSenders[kind], held.sender === sender { return held }
        let made = RtcCeilingSender(sender: sender, queue: Self.libwebrtcQueue)
        ceilingSenders[kind] = made
        return made
    }

    private var ceilingSenders: [CameraSenderKind: RtcCeilingSender] = [:]

    /// The bitrate ceiling each existing camera sender's encoding holds now,
    /// bits a second, for the media log; `nil` as a value when the encoding
    /// holds none. A sender this connection does not have is absent. Read on
    /// the queue the writes run on, so it is what the last write left.
    func cameraSenderCeilings() async -> [CameraSenderKind: Int?] {
        var senders: [(kind: CameraSenderKind, sender: RTCRtpSender)] = []
        for kind in CameraSenderKind.allCases {
            if let sender = cameraSender(kind) { senders.append((kind, sender)) }
        }
        if senders.isEmpty { return [:] }
        return await withCheckedContinuation { continuation in
            Self.libwebrtcQueue.async {
                var out: [CameraSenderKind: Int?] = [:]
                for entry in senders {
                    out[entry.kind] = .some(
                        entry.sender.parameters.encodings.first?.maxBitrateBps?.intValue)
                }
                continuation.resume(returning: out)
            }
        }
    }

    /// Ask a camera sender to keep its frame rate under a bitrate shortfall:
    /// sharpness is what the camera gives up, not motion. Best effort; a
    /// binding that does not take it keeps its default.
    private func preferFramerate(_ sender: RTCRtpSender) {
        let wanted = NSNumber(value: RTCDegradationPreference.maintainFramerate.rawValue)
        SenderParameterWrites.enqueue(sender, on: Self.libwebrtcQueue) { parameters, set in
            if parameters.degradationPreference == wanted { return }
            parameters.degradationPreference = wanted
            _ = set(parameters)
        }
    }

    /// Tell every sender's SFrame encryptor which codec the sender
    /// negotiated (ADR-0071): the first entry of the sender's negotiated
    /// codecs. A video encryptor leaves nothing of a frame clear until it is
    /// told, and an encryptor attached again starts untold, so this runs
    /// after every negotiation and after every attach.
    ///
    /// The read of a sender's parameters blocks on libwebrtc's signaling
    /// thread, so it is made off the main thread; the setter itself is safe
    /// from any thread.
    func syncSenderCodecs() {
        var senders: [(name: String, sender: RTCRtpSender)] = []
        if let sender = localAudioSender { senders.append(("audio", sender)) }
        for kind in CameraSenderKind.allCases {
            if let sender = cameraSender(kind) { senders.append((kind.rawValue, sender)) }
        }
        if senders.isEmpty { return }
        Self.libwebrtcQueue.async {
            let told = senders.map { entry -> String in
                let parameters = entry.sender.parameters
                let codec = parameters.codecs.first?.name
                let set = LakkoSFrameAttach.setCodecName(codec, forSender: entry.sender)
                // The temporal layers the main sender holds once negotiated.
                let layers = entry.name == CameraSenderKind.main.rawValue
                    ? "/t\(parameters.encodings.first?.numTemporalLayers?.intValue ?? 1)" : ""
                return "\(entry.name)=\(codec ?? "-")\(layers)\(set ? "" : "(no encryptor)")"
            }
            NSLog("WebRTCPeerConnection: sender codecs %@", told.joined(separator: " "))
        }
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
        if localVideoTrack != nil { CameraBackgroundStage.shared?.setCameraSending(false) }
        localVideoTrack = nil
        // The copies of an SFU call's camera go with the camera.
        for kind in CameraCopyKind.addOrder {
            removeCameraCopy(kind, reason: "the camera sender was removed")
        }
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
    /// Whether `receiver` is the receiving half of a transceiver this device
    /// sends on (a sender with a track of its own). In an SFU call such a
    /// receiver carries nothing: the SFU forwards every other person on media
    /// lines it opens itself, and only echoes this device's own stream id on
    /// the receiving half of the lines this device offered.
    public func receiverIsOnOwnSender(_ receiver: RTCRtpReceiver) -> Bool {
        guard let peer = peer else { return false }
        return peer.transceivers.contains { transceiver in
            transceiver.receiver == receiver && transceiver.sender.track != nil
        }
    }

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
            (localAudioSender, CameraSender.streamIndexAudio),
            (localVideoSender, CameraSender.streamIndexCamera),
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
            } else {
                rememberSFrame(sender: sender)
            }
        }
        armCameraCopyEncryptors(seed: seed, localLeaf: localLeaf)
        return allOk
    }

    /// Arm SFrame on the copies of an SFU call's camera, each under its own
    /// stream index (3 the light one, 4 the middle one), so no two senders of
    /// this leaf share a key. A copy that cannot be sealed is REMOVED rather
    /// than left unsealed (invariant #4) and the call goes on without it: a
    /// copy is a second picture, never the only one.
    private func armCameraCopyEncryptors(seed: Data, localLeaf: UInt32) {
        for kind in CameraCopyKind.addOrder {
            guard let copy = cameraCopies[kind] else { continue }
            let status = LakkoSFrameAttach.attachEncryptor(
                toSender: copy.sender,
                seed: seed,
                localLeaf: localLeaf,
                streamIndex: CameraSender.streamIndex(of: kind.senderKind)
            )
            if status != .OK {
                removeCameraCopy(kind, reason: "its SFrame encryptor could not be attached")
            } else {
                rememberSFrame(sender: copy.sender)
            }
        }
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
        // Stream 1 (ADR-0035) for the camera's main sender; the copies of an
        // SFU call's camera are sealed under 3 and 4 below, and a shared
        // screen would use 2.
        guard LakkoSFrameAttach.attachEncryptor(
            toSender: sender,
            seed: seed,
            localLeaf: localLeaf,
            streamIndex: CameraSender.streamIndexCamera
        ) == .OK else { return false }
        rememberSFrame(sender: sender)
        armCameraCopyEncryptors(seed: seed, localLeaf: localLeaf)
        return true
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
        if status == .OK, !sframeReceivers.contains(where: { $0 === receiver }) {
            sframeReceivers.append(receiver)
        }
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
    /// the group's epoch during a live call. Walks the senders and
    /// receivers this connection attached a transform to
    /// (``sframeSenders`` / ``sframeReceivers``); a receiver added by a
    /// later renegotiation joins them when it is armed.
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
        guard peer != nil else { return false }
        let now = UInt64(Date().timeIntervalSince1970 * 1000)
        let overlapMs = Self.sframeOverlapMs
        var allOk = true
        for sender in sframeSenders {
            if !LakkoSFrameAttach.rekeyEncryptor(onSender: sender, seed: seed) {
                allOk = false
            }
        }
        for receiver in sframeReceivers {
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
    /// window has elapsed (G-18, ADR-0029), on every receiver armed so far.
    private func pruneSFrameDecryptors() {
        guard peer != nil else { return }
        let now = UInt64(Date().timeIntervalSince1970 * 1000)
        for receiver in sframeReceivers {
            LakkoSFrameAttach.pruneDecryptor(onReceiver: receiver, nowMs: now)
        }
    }

    // MARK: - SDP exchange

    /// Tell every video transceiver to prefer VP8 and its wrapper formats
    /// (`VideoCodecPreference`). Runs before every local description: before
    /// `offer(for:)`, and on the answering side after the remote offer is set
    /// (which is what creates the transceivers for the remote's m-lines) and
    /// before `answer(for:)`. Setting the same list again is harmless.
    ///
    /// Reading the capabilities and the transceivers, and setting the
    /// preferences, all block on libwebrtc's signaling thread, so they run on
    /// `libwebrtcQueue` like every other blocking call here.
    ///
    /// In this binding `setCodecPreferences:` returns nothing, so a list
    /// libwebrtc refuses is not seen here; what is seen is a factory that
    /// offers no VP8 at all, and then every video transceiver is left as it is
    /// and the count is logged once.
    ///
    /// Not covered: a receive-only video transceiver that `offer(for:)` itself
    /// adds for `OfferToReceiveVideo` when none exists yet, which happens only
    /// for a video offer from a device with no usable camera (the camera
    /// track, when there is one, is added before the offer). This device sends
    /// no video on that line, and the answering side chooses what it sends by
    /// its own preferences.
    private func preferVp8(on peer: RTCPeerConnection) async {
        let factory = Self.sharedFactory
        let unset: Int = await withCheckedContinuation { cont in
            Self.libwebrtcQueue.async {
                let video = peer.transceivers.filter { $0.mediaType == .video }
                if video.isEmpty { cont.resume(returning: 0); return }
                let capabilities = factory
                    .rtpReceiverCapabilities(forKind: kRTCMediaStreamTrackKindVideo).codecs
                guard let preferences = VideoCodecPreference.vp8Preferences(
                    capabilities, mimeType: { $0.mimeType })
                else {
                    cont.resume(returning: video.count); return
                }
                for transceiver in video {
                    // webrtc-sdk declares `codecPreferences` as a property whose
                    // setter is `setCodecPreferences:`, so Swift sees only the
                    // property. Only ever written: the binary implements no
                    // getter for it.
                    transceiver.codecPreferences = preferences
                }
                cont.resume(returning: 0)
            }
        }
        if unset > 0 && !vp8Warned {
            vp8Warned = true
            NSLog("WebRTCPeerConnection: %@", VideoCodecPreference.unsetWarning(count: unset))
        }
    }

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
        await preferVp8(on: peer)
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
        await preferVp8(on: peer)
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
        // The remote offer has created this side's transceivers; set the
        // codec before the answer is built from them.
        await preferVp8(on: peer)
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
}

// MARK: - Sender parameter writes

/// The one path every write to an `RTCRtpSender`'s parameters takes; the twin
/// of `apps/web/src/calls/sender-parameters.ts`.
///
/// A write is a read (`sender.parameters`), a change and a set. Two of them
/// on one sender must not overlap, or the second puts back what the first
/// just changed (it read before the first landed). Several parts of the group
/// call write a camera sender's parameters: its temporal layers and
/// frame-rate preference, whether its encoding is active
/// (``SenderActivity``) and its bitrate ceiling (``SenderCeiling``).
///
/// Each hands its change here. The whole read-change-set, and the read-back
/// that follows, run as ONE synchronous block on the serial queue the peer
/// connection's blocking calls use. A serial queue runs one block at a time
/// in the order they were queued, and this binding's getter and setter are
/// synchronous, so on any sender (on all of them, in fact) the writes run one
/// after the other, each on a reading no other write can outdate. The web
/// client needs a promise queue per sender because its `setParameters` is
/// asynchronous; here the block is the transaction.
///
/// Never called on the main thread: the read and the set block on
/// libwebrtc's signaling thread.
enum SenderParameterWrites {

    /// Queue one write of `sender`'s parameters.
    ///
    /// `write` runs on `queue` with the parameters as the sender holds them
    /// at that moment, and with `set`: hand it the changed parameters and it
    /// sets them and returns the sender's parameters read again. The
    /// binding's setter reports nothing, so that reading is where a caller
    /// learns whether the write took. A `write` that never calls `set`
    /// writes nothing.
    static func enqueue(
        _ sender: RTCRtpSender,
        on queue: DispatchQueue,
        write: @escaping (
            _ held: RTCRtpParameters, _ set: (RTCRtpParameters) -> RTCRtpParameters
        ) -> Void
    ) {
        queue.async {
            write(sender.parameters) { changed in
                sender.parameters = changed
                return sender.parameters
            }
        }
    }
}

// MARK: - Sender activity

/// An `RTCRtpSender` as ``SenderActivity`` drives it: one read-modify-set of
/// its first encoding's `isActive`, nothing else. No renegotiation: the media
/// line, the SSRC and the SFrame encryptor stay.
///
/// The read and the write of a sender's parameters block on libwebrtc's
/// signaling thread, so both are made through ``SenderParameterWrites``, on
/// the queue the peer connection's own blocking calls use, never on the main
/// thread. The binding's setter reports nothing, so whether it took is read
/// back.
final class RtcActivitySender: ActivitySender {

    let sender: RTCRtpSender
    private let queue: DispatchQueue

    init(sender: RTCRtpSender, queue: DispatchQueue) {
        self.sender = sender
        self.queue = queue
    }

    func applyEncodingActive(_ active: Bool) async -> SenderActivityOutcome {
        let sender = self.sender
        return await withCheckedContinuation { continuation in
            SenderParameterWrites.enqueue(sender, on: queue) { parameters, set in
                guard let first = parameters.encodings.first else {
                    continuation.resume(returning: .noEncoding)
                    return
                }
                if first.isActive == active {
                    continuation.resume(returning: .applied)
                    return
                }
                first.isActive = active
                let now = set(parameters).encodings.first?.isActive
                continuation.resume(returning: now == active ? .applied : .refused)
            }
        }
    }
}

// MARK: - Sender ceiling

/// An `RTCRtpSender` as ``SenderCeiling`` drives it: one read-modify-set of
/// its first encoding's `maxBitrateBps`, nothing else of the encoding and no
/// renegotiation. Made through ``SenderParameterWrites``, as every other
/// write of the sender's parameters; whether it took is read back
/// (``SenderCeilingWrite``).
final class RtcCeilingSender: CeilingSender {

    let sender: RTCRtpSender
    private let queue: DispatchQueue

    init(sender: RTCRtpSender, queue: DispatchQueue) {
        self.sender = sender
        self.queue = queue
    }

    func applyEncodingCeiling(_ maxBitrateBps: Int) async -> SenderCeilingOutcome {
        let sender = self.sender
        return await withCheckedContinuation { continuation in
            SenderParameterWrites.enqueue(sender, on: queue) { parameters, set in
                let first = parameters.encodings.first
                switch SenderCeilingWrite.step(
                    hasEncoding: first != nil, current: first?.maxBitrateBps?.intValue,
                    wanted: maxBitrateBps)
                {
                case let .done(outcome):
                    continuation.resume(returning: outcome)
                case let .set(bps):
                    first?.maxBitrateBps = NSNumber(value: bps)
                    let readBack = set(parameters).encodings.first?.maxBitrateBps?.intValue
                    continuation.resume(
                        returning: SenderCeilingWrite.outcome(readBack: readBack, wanted: bps))
                }
            }
        }
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
        guard let removed = rtpReceiver.track as? RTCVideoTrack else {
            return
        }
        let trackId = removed.trackId
        Task { @MainActor [weak self] in
            self?.delegate?.onRemoteVideoTrackRemoved(trackId)
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
