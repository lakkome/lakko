// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore
import WebRTC

/// 1×1 P2P call session on iOS.
///
/// Mirror of `apps/web/src/calls/call-session.ts` translated
/// into a `@MainActor` `ObservableObject`. Tracks the call
/// state machine + composes outbound signaling via the
/// UniFFI `composeCall*Signaling` free functions.
///
/// ## G3-1 status
///
/// Real WebRTC integration: `dial()` / `acceptCall()`
/// build real SDP via [`WebRTCPeerConnection`], local ICE
/// candidates are emitted through the `iceCandidate`
/// signaling body, and the underlying RTCPeerConnection's
/// connection-state transitions drive `WebCallState`.
/// Video capture is wired (G3-1-video): a video-enabled
/// `dial` / `acceptCall` constructs the camera pipeline
/// (`startLocalCameraCapture`), adds the track via
/// `WebRTCPeerConnection.addLocalVideoTrack`, and surfaces
/// it through the published `localVideoTrack` /
/// `remoteVideoTrack` slots.
///
/// ## SFrame
///
/// Media is end-to-end encrypted via SFrame keyed off the
/// call's MLS exporter (G3-sframe-native, G-28). `dial()` /
/// `acceptCall()` call `attachSFrame` after adding the
/// senders and **refuse the call** if the attach fails
/// (security invariant #4: no fallback to non-E2EE);
/// `e2eeActive` flips true once `setCallKey` arms the seed.
@MainActor
public final class CallSession: ObservableObject {

    /// Lifecycle phases (mirror of `lakko_calls::CallState`).
    public enum WebCallState: String {
        case idle, outgoing, ringing, connecting, connected, ending, done
    }

    /// Reason carried in the terminal transition.
    public enum WebCallEndReason: String {
        case hungUp, busy, declined, timedOut, error
    }

    /// Lifecycle of a mid-call camera-on (piece B2a) — the 1×1 analog of
    /// `SfuCallSession.VideoPublishState`. A 1×1 call started audio-only has
    /// no video sender, so ``addLocalVideo()`` ACQUIRES the camera + adds a
    /// track + renegotiates; that acquire is async, so the UI shows a
    /// camera-button spinner + a self-tile "Starting camera…" hint while it
    /// is in flight:
    ///   * `off`      — no video published (the default; also after a
    ///                  failure the caller reverted, and at teardown).
    ///   * `starting` — camera capture + the addTrack/renegotiation are in
    ///                  flight.
    ///   * `live`     — the local video track is captured, SFrame-sealed,
    ///                  and added.
    ///   * `failed`   — no camera / permission denied / capture error / the
    ///                  SFrame arm failed; the caller reverts the toggle +
    ///                  surfaces a message. The call stays up audio-only
    ///                  (invariant #4 — no half-added / unsealed sender is
    ///                  ever offered to the peer).
    public enum VideoPublishState: String, Equatable {
        case off, starting, live, failed
    }

    /// 16-byte call_id minted by the offerer.
    public let callId: Data

    @Published public private(set) var state: WebCallState = .idle
    @Published public private(set) var endReason: WebCallEndReason?

    /// Hands a composed frame to the publisher, with whether that frame asks
    /// for a call push wake (``CallSignalingWake``). The session decides,
    /// because only it knows whether an offer is the dial or a renegotiation
    /// and whether the call ever connected; the bytes alone say neither.
    private let onOutboundSignaling: (_ bytes: Data, _ wake: Bool) -> Void
    private var isOfferer = false
    /// The call reached `.connected` on this device at least once. An `end`
    /// sent after that asks for no wake: everyone it concerns is in the call.
    private var everConnected = false

    /// Perfect-negotiation state (mid-call 1×1 add-track, piece
    /// B2b — the web `call-session.ts` mirror). The OFFERER
    /// (caller) is the impolite peer, the ANSWERER (callee) is
    /// polite (`polite = !isOfferer`). `negotiationReady` gates
    /// the mid-call renegotiation drive off until the initial
    /// offer/answer has been applied (it flips true after the
    /// initial answer on the offerer side / after the initial
    /// answer is sent on the answerer side), so the manual
    /// initial exchange is never duplicated. `makingOffer` is
    /// true only while a local renegotiation offer is being
    /// built + shipped; `isSettingRemoteAnswerPending` bridges
    /// the window while a mid-call answer is being applied — both
    /// feed the glare guard in `applyRenegotiationOffer`.
    /// `addingVideo` is the in-flight guard for `addLocalVideo`.
    private var makingOffer = false
    private var isSettingRemoteAnswerPending = false
    private var negotiationReady = false
    /// Remote ICE candidates held until the remote description is set (a
    /// ringing call has no peer connection yet).
    private var remoteCandidates = RemoteCandidateBuffer()
    private var addingVideo = false

    /// Stored remote SDP offer between `.ringing` → `.connecting`.
    /// `acceptCall` reads it to build the answer.
    private var pendingRemoteOfferSdp: String?

    /// The WebRTC pod. Lazily constructed inside `dial` /
    /// `acceptCall` so an `idle` session pays nothing.
    private var rtc: WebRTCPeerConnection?

    /// Cached audio toggle for `dial`-side construction.
    private var dialAudio: Bool = true
    private var dialVideo: Bool = false

    /// G3-1-video-ui — local-camera capture lifecycle. Held
    /// for the duration of a video-enabled call so the
    /// AVCaptureSession + RTCVideoSource pair survives a
    /// SwiftUI view recompose. Nil when the call is audio-
    /// only or before `completeDial` / `completeAccept` has
    /// fired.
    private var localCameraCapture: LocalCameraCapture?

    /// G3-1-video-ui — observable for the SwiftUI preview
    /// overlay. The CallScreenView reads this via @ObservedObject
    /// + `if let track = session.localVideoTrack { LocalVideoPreviewView(track: track) }`.
    /// Set inside `completeDial` / `completeAccept` after
    /// `addLocalVideoTrack`; cleared in `endLocal`.
    @Published public private(set) var localVideoTrack: RTCVideoTrack?

    /// G3-1-remote-video — the remote participant's incoming
    /// video track, surfaced from `WebRTCPeerConnection`'s
    /// `onRemoteVideoTrack` delegate callback. `nil` until the
    /// remote adds a camera track (or after it is removed /
    /// the call ends). On the 1×1 direct-PeerConnection path
    /// there is exactly one remote, so this single handle is
    /// unambiguous; the N-way SFU path (per-participant
    /// MSID→leaf mapping) is a deferred follow-up that will
    /// replace this with a `[participantId: RTCVideoTrack]`
    /// map keyed on the stream id the SFU stamps.
    @Published public private(set) var remoteVideoTrack: RTCVideoTrack?

    /// `true` once the current ``remoteVideoTrack`` has actually rendered a
    /// frame. A track handle arrives with the SDP, a picture does not — see
    /// ``RemoteVideoFrameProbe``. The call screen keys its layout on THIS, so a
    /// peer that announces video but sends nothing openable leaves the portrait
    /// up rather than a dark rectangle with no controls on it.
    @Published public private(set) var remoteVideoFlowing: Bool = false

    /// `true` once arming the SFrame decryptor on one of the peer's receivers
    /// failed. That receiver decodes nothing — the crypto options declare frame
    /// encryption required, so libwebrtc discards its frames rather than
    /// rendering them — and the refusal is correct; what it is not, without
    /// this, is visible. Blank video and silence are what a bad network looks
    /// like too, so the screen reads this and says "can't decrypt" instead.
    ///
    /// Sticky: a receiver that failed to arm is never re-armed.
    @Published public private(set) var remoteMediaUndecryptable: Bool = false

    /// The other person's shared screen (ADR-0083 section 4): a remote video
    /// track in a stream whose id ends in `~screen`. Its own picture, never
    /// their camera.
    @Published public private(set) var remoteScreenTrack: RTCVideoTrack?
    /// `true` once ``remoteScreenTrack`` rendered a frame; until then the share
    /// says it is waiting for the picture.
    @Published public private(set) var remoteScreenFlowing: Bool = false
    /// The call's current share, as the fold answers it: `nil` when none is
    /// live. Fed only with states verified against the MLS sender
    /// (``applyScreenShare(_:)``).
    @Published private(set) var screenShare: CallCurrentScreenShare?
    /// The one-share-at-a-time fold (C-87). Its own key decides only whether
    /// a share is this device's own; this client never shares, so a key no
    /// signer holds is enough.
    private lazy var screenShares = ScreenShareFold(ownKey: Data(count: 32))
    private var remoteScreenProbe: RemoteVideoFrameProbe?
    /// The id of the remote video track the pod last reported removed.
    private var lastRemovedVideoTrackId: String?

    /// The frame probe attached to the current remote video track, held so it
    /// can be detached when the track is replaced or the call ends.
    private var remoteVideoProbe: RemoteVideoFrameProbe?
    private var probedRemoteVideoTrack: RTCVideoTrack?
    /// The one-shot inbound-stats read armed when a remote video track attaches.
    private var remoteVideoDiagnosisTask: Task<Void, Never>?

    /// G3-1-call-controls — observable local mic / camera
    /// enabled state, so the in-call control buttons reflect
    /// (and drive) the real WebRTC track `isEnabled` flags
    /// rather than a mock toggle. `micEnabled` defaults true
    /// (calls start unmuted); `cameraEnabled` defaults false
    /// and flips true when the camera pipeline arms on a
    /// video call. `CallUiState.driveFrom` maps these onto the
    /// local participant's `micOn` / `camOn`.
    @Published public private(set) var micEnabled: Bool = true
    @Published public private(set) var cameraEnabled: Bool = false

    /// What the other person last said about their microphone and camera
    /// (the `mediaState` call signal, 2026-11-07). Both `true` until the
    /// first one arrives, and again when the call ends. A mute is a track
    /// `isEnabled = false` on their side, which the receiving track never
    /// reports, so this is the only way the remote tile learns of it
    /// (`docs/ux/group-call-grid.md` 7.2). `CallUiState.driveFrom` maps them
    /// onto the remote participant's `micOn` / `camOn`.
    @Published public private(set) var remoteMicOn: Bool = true
    @Published public private(set) var remoteCameraOn: Bool = true

    /// The camera and microphone state last told to the other side.
    private var mediaAnnouncement = OneToOneMediaAnnouncement()

    /// B2a — mid-call camera-on lifecycle. Starts `.off`;
    /// ``addLocalVideo()`` flips it `.starting` while the camera
    /// acquire + renegotiation is in flight, then `.live` on
    /// success or `.failed` on any thrown error; teardown resets
    /// it `.off`. `CallUiState.driveFrom` mirrors it so the
    /// camera button shows a spinner (+ disables) and the local
    /// self-tile shows a "Starting camera…" hint while `.starting`.
    @Published public private(set) var videoPublishState: VideoPublishState = .off

    /// G3-1-e2ee-active — `true` once `setCallKey` has armed the
    /// SFrame seed (the call's media is end-to-end encrypted).
    /// `CallUiState.driveFrom` maps this onto the E2EE pill so
    /// it shows the green lock only when keying is real, not
    /// optimistically.
    @Published public private(set) var e2eeActive: Bool = false

    /// Canonical call safety number — `callSafetyNumber(seed)` over the
    /// shared per-call seed, set alongside `e2eeActive` when the key is
    /// armed. The same digits every participant derives; the UI shows it
    /// in the E2EE pill for an out-of-band man-in-the-middle check. `nil`
    /// until the key is armed (or if the derivation fails — display-only,
    /// never fatal).
    @Published public private(set) var safetyNumber: String?

    /// G3-1-conn-quality — live connection stats, polled from
    /// the pod's `RTCStatisticsReport` every couple of seconds
    /// while the call is connected. `rttMs` is `nil` until ICE
    /// settles + WebRTC has an RTT estimate; `connectionRelayed`
    /// is `true` when the active candidate-pair goes through a
    /// TURN relay. `CallUiState.driveFrom` maps these onto the
    /// header quality-dot + the RTT/connection-type popover.
    @Published public private(set) var rttMs: Int?
    @Published public private(set) var connectionRelayed: Bool = false

    /// G3-1-conn-quality iter2 — receive / send bitrate in **bits
    /// per second** (the popover's `↓`/`↑` lines), derived from the
    /// candidate-pair's cumulative byte counters diffed across two
    /// poll samples against the wall clock. `nil` until a second
    /// sample lands (a single sample carries no rate).
    @Published public private(set) var downBps: Int?
    @Published public private(set) var upBps: Int?

    /// Previous (bytesReceived, bytesSent, monotonic timestamp in
    /// seconds) used to derive [`downBps`] / [`upBps`]. Reset in
    /// `teardownMediaPipeline` so a fresh call starts clean.
    private var prevStatsBytes: (received: Int64, sent: Int64, atSec: Double)?

    /// Background poll loop driving [`rttMs`] / [`connectionRelayed`]
    /// / [`downBps`] / [`upBps`]. Started on the first `.connected`
    /// transition, cancelled in `teardownMediaPipeline`.
    private var statsTask: Task<Void, Never>?
    /// What the receivers report, logged with every stats sample and as a
    /// total in `teardownMediaPipeline` (ADR-0067 step 1, as the group call
    /// does).
    private var inboundCounters = InboundCounterLog()
    /// The media-flow line is said once per call, not every two seconds.
    private var loggedMediaFlow = false

    /// The `a=ssrc:` this device announced in its own description, as text.
    /// Paired with the SSRC actually sent: a peer can only demux what was
    /// announced, so the two differing is a call whose media is dropped on
    /// arrival while every other signal says the transport is healthy.
    static func signalledSsrc(sdp: String) -> String {
        for line in sdp.split(separator: "\r\n") where line.hasPrefix("a=ssrc:") {
            let rest = line.dropFirst("a=ssrc:".count)
            return String(rest.split(separator: " ").first ?? "")
        }
        return "none"
    }
    /// How long inbound media has been silent (`MediaWatchdog`).
    private var inboundActivity: MediaWatchdog.InboundActivity?
    /// Which network path the media takes, logged when it is first known
    /// and when it changes (``CallPath``).
    private var callPathLog = CallPathLog()
    /// The ICE-`disconnected` grace, armed on the transition and cancelled by
    /// a recovery. See `handleConnectionState`.
    private var iceGraceTask: Task<Void, Never>?
    /// The last transport state seen, so the grace can ask what it decided on
    /// without reaching into the pod for a state it does not publish.
    private var lastConnectionState: RTCPeerConnectionState = .new

    /// A CONNECTED call that has received nothing for this long is over
    /// (`MediaWatchdog`). Chosen, not measured: several times any
    /// renegotiation gap, and short enough that a call whose peer has gone
    /// does not sit there counting time.
    private static let mediaSilenceTimeoutSeconds: TimeInterval = 30

    /// ICE `.disconnected` is often transient (a Wi-Fi roam, a handover); the
    /// call ends only if it has not recovered after this. The same window the
    /// web uses. `.failed` still ends the call at once.
    private static let iceDisconnectedGraceSeconds: TimeInterval = 10

    /// G3-sframe-native N-6 — per-call MLS exporter seed +
    /// local participant leaf index. Both must be set before
    /// `dial` / `acceptCall` so the SFrame attach can fire
    /// against every fresh RTCRtpSender immediately after the
    /// track is added. A nil seed at attach time aborts the
    /// call (security invariant #4: no fallback to non-E2EE).
    private var callKeySeed: Data?
    private var localLeaf: UInt32?

    /// G3-1-turn — TURN-credential client. Injected by the
    /// call-UI layer via [`setTurnClient`] so tests can swap
    /// a stub. When `nil` (or when it mints nothing) the
    /// call proceeds on host candidates only — fail-soft
    /// path per `TurnCredentialClient` docs.
    private var turnClient: TurnCredentialClient?

    /// Bind the TURN client used for ICE-server resolution at
    /// `dial` / `acceptCall` time. Idempotent; passing `nil`
    /// disables TURN fetching for subsequent calls (host
    /// candidates only).
    public func setTurnClient(_ client: TurnCredentialClient?) {
        self.turnClient = client
    }

    public init(callId: Data, onOutboundSignaling: @escaping (_ bytes: Data, _ wake: Bool) -> Void) {
        self.callId = callId
        self.onOutboundSignaling = onOutboundSignaling
        // ADR-0089: the camera background stopped working. Nothing has been
        // sent since; the camera goes off here, as the camera button turns it
        // off, whether or not the call screen is showing.
        backgroundFailure = CameraBackground.shared.$failures
            .dropFirst()
            .sink { [weak self] _ in
                guard let self, self.cameraEnabled else { return }
                self.setCameraEnabled(false)
            }
    }

    /// The camera background's failures (ADR-0089).
    private var backgroundFailure: AnyCancellable?

    /// Inject the per-call SFrame keying material. Caller MUST
    /// run this AFTER the welcome-ferry / KP-forward dispatcher
    /// has resolved (for joiners) or AFTER `Group.create`
    /// on the 1×1-from-chat path (for the call originator),
    /// and BEFORE `dial` / `acceptCall`.
    ///
    /// `seed` is exactly 32 bytes
    /// (`Group.export_secret("lakko/call/v1", call_id, 32)`).
    /// `localLeaf` is this device's leaf index in the call's
    /// MLS group.
    public func setCallKey(seed: Data, localLeaf: UInt32) {
        self.callKeySeed = seed
        self.localLeaf = localLeaf
        // G3-1-e2ee-active — the SFrame seed is now present, so
        // the call's E2EE keying is armed. The UI's E2EE pill
        // mirrors this (green-lock vs negotiating).
        self.e2eeActive = true
        // The safety number is a canonical hash of the same seed —
        // shown in the pill for an out-of-band MITM check. Display-only:
        // a derivation failure (wrong-length seed, which the exporter
        // never emits) leaves it nil and never breaks the call.
        self.safetyNumber = try? callSafetyNumber(seed: seed)
    }

    /// Generate a fresh 16-byte call_id.
    public static func mintCallId() -> Data {
        var bytes = Data(count: 16)
        _ = bytes.withUnsafeMutableBytes { ptr -> OSStatus in
            guard let base = ptr.baseAddress else { return -1 }
            return SecRandomCopyBytes(kSecRandomDefault, 16, base)
        }
        return bytes
    }

    // MARK: - Outbound

    /// Outbound side: dial the contact. Spins up
    /// `WebRTCPeerConnection`, attaches a local audio track,
    /// generates a real SDP offer and ships it via the
    /// existing signaling pipe.
    public func dial(audio: Bool, video: Bool) {
        guard state == .idle else { return }
        isOfferer = true
        dialAudio = audio
        dialVideo = video
        // A call placed without audio starts muted, and says so.
        if !audio { micEnabled = false }
        state = .outgoing

        // G3-1-turn — fetch TURN credential before constructing
        // the RTCPeerConnection so its initial RTCConfiguration
        // already lists the relay candidate. Doing it after
        // construction would require an `setConfiguration:` call
        // mid-flight, which the WebRTC pod accepts but
        // some libwebrtc forks have known footguns around (the
        // ICE agent may have already started gathering, in which
        // case the new TURN server is ignored).
        Task { [weak self] in
            guard let self = self else { return }
            let iceServers = await self.resolveIceServers()
            await MainActor.run {
                self.completeDial(iceServers: iceServers, audio: audio, video: video)
            }
        }
    }

    private func completeDial(iceServers: [RTCIceServer], audio: Bool, video: Bool) {
        // The call may have ended while the relay credential was fetched (the
        // person hung up, or the establish timer fired). The teardown ran
        // with no peer connection to close, so building one now would start
        // the microphone and the camera for a call that is over, ship its
        // offer after its End, and leave nothing that would ever stop them.
        guard state == .outgoing else { return }
        let rtc = WebRTCPeerConnection(iceServers: iceServers)
        guard rtc.isReady else {
            NSLog("CallSession.dial: RTCPeerConnection allocation failed")
            endLocal(reason: .error)
            return
        }
        installDelegate(on: rtc)
        // A voice call starts on the earpiece, as it always has; a call with
        // video plays on the loudspeaker unless a headset or a Bluetooth
        // device is connected.
        CallAudioSession.configureCallOutput(loudspeakerByDefault: video)
        CallAudioSession.describe("dial: before local audio track")
        // A mute chosen before the capture (the CallKit mute, the in-call
        // button while dialling) holds for the captured microphone: what
        // the other side is told is what is sent.
        rtc.addLocalAudioTrack(enabled: micEnabled)

        // G3-1-video-ui — local video track + camera capture.
        // MUST land BEFORE `composeAndShipOffer` because the
        // SDP build pipeline counts senders and emits the
        // m=video line based on what `RTCPeerConnection`
        // already has attached. Adding the track later would
        // force a renegotiation we do not drive today.
        if video {
            startLocalCameraCapture(on: rtc)
        }

        // G3-sframe-native N-6 — arm SFrame on every freshly-
        // added sender. Must happen AFTER addLocalAudioTrack
        // (it captures the RtpSender we need to attach to) and
        // BEFORE composeAndShipOffer (the SDP-build path can
        // run on a codec thread the moment the sender exists,
        // so we want the encryptor already in place to avoid
        // a window where libwebrtc would tee bytes unencrypted).
        if !attachSFrame(on: rtc) {
            NSLog("dial: SFrame attach failed — refusing per security invariant #4")
            endLocal(reason: .error)
            return
        }
        self.rtc = rtc

        Task { [weak self] in
            await self?.composeAndShipOffer(rtc: rtc, audio: audio, video: video)
        }
    }

    /// G3-1-video-ui — common helper for the dial + accept
    /// paths. Constructs the camera capture pipeline, attaches
    /// the resulting track to the peer connection, and starts
    /// the capturer asynchronously. Soft-fail if no camera is
    /// present (e.g. simulator without a host camera, Mac
    /// Catalyst without webcam) — the call still proceeds as
    /// audio-only rather than aborting.
    ///
    /// The capturer is held on `self.localCameraCapture` so
    /// it survives the synchronous return of this method and
    /// gets torn down in `endLocal`.
    private func startLocalCameraCapture(on rtc: WebRTCPeerConnection) {
        guard LocalCameraCapture.hasUsableCamera() else {
            NSLog("CallSession: no camera available; staying audio-only")
            return
        }
        let source = WebRTCPeerConnection.newVideoSource()
        guard let capture = LocalCameraCapture(source: source) else {
            NSLog("CallSession: LocalCameraCapture init returned nil")
            return
        }
        rtc.addLocalVideoTrack(source)
        self.localCameraCapture = capture
        // The track lives on the peer connection; expose it
        // to SwiftUI via the published `localVideoTrack` so
        // the call screen can render the preview overlay.
        self.localVideoTrack = rtc.currentLocalVideoTrack()
        // G3-1-call-controls — camera is now live; the in-call
        // camera toggle reflects + drives this from here.
        self.cameraEnabled = true
        // Start capture off the main actor — `startCapture`
        // can take a few hundred ms while `AVCaptureSession`
        // negotiates with the camera service.
        Task { [weak self] in
            do {
                try await capture.start()
            } catch {
                NSLog(
                    "CallSession: camera start failed: %@",
                    String(describing: error)
                )
                await MainActor.run {
                    // On a camera-start failure, drop the
                    // capture handle + clear the track ref
                    // so the UI hides the preview overlay.
                    // The call proceeds audio-only — the
                    // already-added (but now silent) video
                    // sender keeps the SDP m=video line
                    // intact, which is fine for the receiver
                    // (they will see a black frame).
                    //
                    // Only while this capture is still the call's: after
                    // the call ended there is nothing of it left to clear.
                    guard let self, self.localCameraCapture === capture else { return }
                    self.localCameraCapture = nil
                    self.localVideoTrack = nil
                }
            }
        }
    }

    /// Build the ICE-server list for this call: Lakko's own
    /// servers only (``IceServers``) — the TURN row minted via
    /// the auth service and STUN on its host, or an empty list
    /// (host candidates only) when `turnClient` is unset or
    /// mints nothing. The call still attempts to connect then;
    /// two peers behind NATs may not reach each other, and that
    /// surfaces as a normal ICE-disconnected state rather than
    /// a refused call.
    private func resolveIceServers() async -> [RTCIceServer] {
        await IceServers.resolve(turnClient)
    }

    /// Resolve the seed + leaf and invoke
    /// `armSFrameEncryptors`. Returns `false` when:
    ///   - `setCallKey` was never called (caller forgot — bug)
    ///   - `SFrameAvailability.isCallingAvailable` is false
    ///   - any sender failed to attach
    /// All three cases collapse to a refused call.
    private func attachSFrame(on rtc: WebRTCPeerConnection) -> Bool {
        guard let seed = callKeySeed, let leaf = localLeaf else {
            NSLog("attachSFrame: setCallKey was never called — security invariant #4")
            return false
        }
        guard SFrameAvailability.shared.isCallingAvailable else {
            NSLog("attachSFrame: SFrameAvailability reports calling unavailable")
            return false
        }
        return rtc.armSFrameEncryptors(seed: seed, localLeaf: leaf)
    }

    private func composeAndShipOffer(
        rtc: WebRTCPeerConnection,
        audio: Bool,
        video: Bool
    ) async {
        do {
            let offerSdp = try await rtc.createOffer(audio: audio, video: video)
            // Ended while the offer was built: an offer sent now would
            // follow the call's End and ring the other person for a call
            // that is over.
            guard self.rtc === rtc else { return }
            let bytes = try composeCallSignaling(
                body: .offer(
                    callId: callId,
                    sdp: offerSdp,
                    createdAtMs: UInt64(Date().timeIntervalSince1970 * 1000),
                    media: FfiCallMediaFlags(audio: audio, video: video)
                )
            )
            // The dial: the one offer that rings the other person.
            onOutboundSignaling(
                bytes, CallSignalingWake.sessionAsks(.dialOffer, everConnected: everConnected))
        } catch {
            // A build that failed because the call ended meanwhile is not an
            // error of the call.
            guard self.rtc === rtc else { return }
            NSLog("dial offer-build failed: %@", String(describing: error))
            endLocal(reason: .error)
        }
    }

    /// G3-1-call-controls — mute / unmute the local mic. Drives
    /// the real `RTCAudioTrack.isEnabled` via the pod and
    /// publishes the new state so the in-call control reflects
    /// it. No-op-safe before the pod exists (the published flag
    /// still updates so the calling / connecting UI is correct).
    public func setMicrophoneEnabled(_ on: Bool) {
        rtc?.setMicrophoneEnabled(on)
        micEnabled = on
        // The other side's tile shows the mute mark from this.
        announceMediaStateIfChanged()
    }

    /// Whether this device sends a picture: a camera track that exists and
    /// is switched on.
    private var localCameraIsOn: Bool {
        cameraEnabled && localVideoTrack != nil
    }

    /// Tell the other side this device's camera and microphone state (the
    /// one-to-one `mediaState` call signal, 2026-11-07) when it differs from
    /// what was last told. Only on a connected call; the transition to
    /// `connected` sends the first one. Never a wake. Best-effort: a compose
    /// failure is swallowed, since it is a UI hint and never on the media
    /// path.
    private func announceMediaStateIfChanged() {
        // A connected call whose peer connection is gone is being torn
        // down (the other side ended it): nothing more is said.
        guard let next = mediaAnnouncement.next(
            connected: state == .connected && rtc != nil,
            camera: localCameraIsOn, microphone: micEnabled),
            let bytes = try? composeCallSignaling(
                body: .mediaState(callId: callId, cameraOn: next.camera, microphoneOn: next.microphone))
        else { return }
        onOutboundSignaling(
            bytes, CallSignalingWake.sessionAsks(.mediaState, everConnected: everConnected))
    }

    /// G3-1-call-controls — start / stop the local camera track.
    /// Toggles `RTCVideoTrack.isEnabled` via the pod (no SDP
    /// renegotiation) and publishes the new state.
    public func setCameraEnabled(_ on: Bool) {
        rtc?.setCameraEnabled(on)
        cameraEnabled = on
        announceMediaStateIfChanged()
        guard !on, let rtc else { return }
        // `docs/ux/calls.md` section 4.1 — the call returns to the portrait on
        // BOTH screens. Disabling the track is invisible to the peer (libwebrtc
        // goes on transmitting black frames and fires nothing over there), so
        // the transceiver stops offering video and one renegotiation carries
        // that across. The sender itself stays, so its SFrame encryptor and
        // frame counter survive the toggle — see `setVideoSendingEnabled`.
        setVideoPublishState(.off)
        guard rtc.setVideoSendingEnabled(false) else { return }
        Task { @MainActor [weak self] in
            guard let self, let rtc = self.rtc else { return }
            await self.driveRenegotiationOffer(rtc: rtc, video: false)
        }
    }

    /// `docs/ux/calls.md` section 4.1 — is there another camera to flip to?
    /// The flip button lives on the self-view and is not drawn when this is
    /// `false`, rather than drawn and inert.
    public var canFlipCamera: Bool {
        localCameraCapture?.canFlip ?? false
    }

    /// Swap the front camera for the back one and back again, on the same
    /// track and the same sender — so the SFrame encryptor attached to it
    /// keeps its counter and only the frames change.
    public func flipCamera() async {
        guard let capture = localCameraCapture else { return }
        await capture.flip()
    }

    /// Turn the camera ON mid-call (piece B2b) when the 1×1 call
    /// started audio-only. Mirror of the web
    /// `CallSession.addLocalVideo`.
    ///
    /// Re-enables an existing live video sender (a call that
    /// started as video, or a prior `addLocalVideo`) — no capture,
    /// no renegotiation; else captures the front camera (720p/30),
    /// adds the track, SFrame-seals the new video sender, and drives
    /// a renegotiation offer directly (like the SFU mirror — NOT via
    /// a negotiation-needed callback). The 1×1 media IS SFrame-sealed
    /// (the audio sender is armed at connect via `attachSFrame`), so
    /// the mid-call video sender must be sealed with the SAME per-call
    /// seed + leaf, or the peer's decryptors — which expect it sealed
    /// — would fail. Only the NEW video sender is armed
    /// (`armSFrameVideoEncryptor`); re-running the full
    /// `armSFrameEncryptors` would reset the audio encryptor's counter
    /// (nonce reuse).
    ///
    /// Idempotent + in-flight-guarded, so a redundant reactive tick
    /// is a cheap no-op. Fail-closed (invariant #4): on a no-camera /
    /// camera-start failure — OR when the SFrame arm cannot complete
    /// (missing seed/leaf, calling unavailable, or the attach fails)
    /// — the just-added video sender is removed, the capture is torn
    /// down, the local-video state is reset, and it throws BEFORE any
    /// renegotiation, so an UNSEALED video m-line is never offered to
    /// the peer. The caller reverts the toggle + surfaces a message;
    /// the call stays up audio-only.
    ///
    /// - Throws: ``CallSessionError/noCamera`` when the host has no
    ///   camera, ``CallSessionError/sframeUnavailable`` when the
    ///   video sender cannot be SFrame-sealed, or the underlying
    ///   `LocalCameraCapture.start` error on a camera-start failure.
    public func addLocalVideo() async throws {
        guard let rtc = rtc else { return }
        // A live video sender already exists — just re-enable it. This is the
        // path a camera toggled off and on again takes, and the reason the
        // toggle does not remove the sender: re-enabling reuses the SAME
        // encryptor, so the frame counter carries on from where it stopped
        // rather than restarting at zero over a key that has already been used
        // (ADR-0035). If the off-toggle had stopped the transceiver offering
        // video, one renegotiation puts it back.
        if rtc.currentLocalVideoTrack() != nil {
            rtc.setCameraEnabled(true)
            cameraEnabled = true
            CallAudioSession.callGainedVideo()
            setVideoPublishState(.live)
            if rtc.setVideoSendingEnabled(true) {
                await driveRenegotiationOffer(rtc: rtc, video: true)
            }
            return
        }
        if addingVideo { return } // capture in flight — do not double-acquire
        addingVideo = true
        // B2a — the camera acquire + renegotiation is now in flight; the UI
        // shows the camera-button spinner + the self-tile hint until this
        // resolves to `.live` / `.failed`.
        setVideoPublishState(.starting)

        // Acquire the camera FIRST, before adding any track, so a
        // capture failure never leaves a half-added sender on the
        // peer connection.
        guard LocalCameraCapture.hasUsableCamera() else {
            addingVideo = false
            setVideoPublishState(.failed)
            throw CallSessionError.noCamera
        }
        let source = WebRTCPeerConnection.newVideoSource()
        guard let capture = LocalCameraCapture(source: source) else {
            addingVideo = false
            setVideoPublishState(.failed)
            throw CallSessionError.noCamera
        }
        do {
            try await capture.start()
        } catch {
            // The call ended while the camera was starting: its teardown
            // already reset the state, and a camera that did not start for a
            // call that is over is not a failure to show.
            guard self.rtc === rtc else { return }
            addingVideo = false
            setVideoPublishState(.failed)
            throw error
        }

        // Re-read the pod AFTER the async capture — the call may
        // have ended while the camera was starting.
        guard let liveRtc = self.rtc else {
            await capture.stop()
            addingVideo = false
            // The call went away mid-acquire — not a failure to surface;
            // teardown already reset the state (or is about to).
            setVideoPublishState(.off)
            return
        }
        liveRtc.addLocalVideoTrack(source)
        // Seal the mid-call video sender with SFrame using the SAME
        // per-call seed + leaf the audio sender was armed with at
        // connect (invariant #4). Fail-closed: on any inability to
        // seal, remove the sender + tear the capture down + reset
        // state + throw BEFORE renegotiating, so an unsealed video
        // m-line never reaches the peer. Mirrors the SFU
        // `publishVideo` fail-closed arm.
        let sealed: Bool
        if let seed = callKeySeed, let leaf = localLeaf,
            SFrameAvailability.shared.isCallingAvailable {
            sealed = liveRtc.armSFrameVideoEncryptor(seed: seed, localLeaf: leaf)
        } else {
            sealed = false
        }
        guard sealed else {
            liveRtc.removeLocalVideoTrack()
            self.localCameraCapture = nil
            self.localVideoTrack = nil
            self.cameraEnabled = false
            addingVideo = false
            await capture.stop()
            // The call may have ended while the camera stopped; the teardown
            // already put the state back to `.off`.
            if self.rtc === liveRtc { setVideoPublishState(.failed) }
            throw CallSessionError.sframeUnavailable
        }
        self.localCameraCapture = capture
        self.localVideoTrack = liveRtc.currentLocalVideoTrack()
        // The camera is now live; the in-call toggle + the local
        // tile reflect this via the published `cameraEnabled` /
        // `localVideoTrack` (mapped in `CallUiState.driveFrom`).
        self.cameraEnabled = true
        addingVideo = false
        // The call is a video call now: its sound moves to the loudspeaker
        // unless a headset or a Bluetooth device is connected.
        CallAudioSession.callGainedVideo()
        // B2a — camera is live; the spinner clears (the UI only shows it
        // while `.starting`).
        setVideoPublishState(.live)
        // Renegotiate so the peer learns the new (sealed) video m-line.
        await driveRenegotiationOffer(rtc: liveRtc)
    }

    /// B2a — publish the mid-call camera-on lifecycle. Guards a redundant
    /// re-publish so `@Published` doesn't fire on an unchanged value.
    private func setVideoPublishState(_ next: VideoPublishState) {
        guard videoPublishState != next else { return }
        videoPublishState = next
        // A camera published (or lost) mid-call is a camera state change.
        announceMediaStateIfChanged()
    }

    /// Outbound side: tear down a call we initiated or
    /// inbound side: tear down an active call.
    public func hangUp(reason: WebCallEndReason = .hungUp) {
        endLocal(reason: reason)
    }

    /// Inbound side: accept a ringing call. Builds the SDP
    /// answer against the stored remote offer + ships it.
    ///
    /// `withVideo` mirrors the offerer's `dial(video:)` —
    /// the call surface decides per-call whether the
    /// answerer publishes video. Defaults to `false`: an
    /// answerer can accept a video call audio-only, the
    /// SDP answer simply omits the video m-line's
    /// `recvonly` direction. See `G3-1-video-ui` docs in
    /// ROADMAP for the deferred "video toggle" sub-sprint.
    public func acceptCall(withVideo: Bool = false) {
        guard state == .ringing, !isOfferer else { return }
        guard let offerSdp = pendingRemoteOfferSdp else {
            NSLog("acceptCall: pendingRemoteOfferSdp missing — race?")
            endLocal(reason: .error)
            return
        }

        // G3-1-turn — same async TURN-fetch shape as `dial()`.
        Task { [weak self] in
            guard let self = self else { return }
            let iceServers = await self.resolveIceServers()
            await MainActor.run {
                self.completeAccept(
                    iceServers: iceServers,
                    offerSdp: offerSdp,
                    withVideo: withVideo
                )
            }
        }
    }

    private func completeAccept(
        iceServers: [RTCIceServer],
        offerSdp: String,
        withVideo: Bool
    ) {
        // The call may have ended while the relay credential was fetched: the
        // caller hung up, the ring timed out, or it was declined. Without
        // this an ended call came back as `connecting`, with the microphone
        // (and the camera) on. It also makes a second accept of the same
        // ring a no-op: the first one has already left `ringing`.
        guard state == .ringing else { return }
        let rtc = WebRTCPeerConnection(iceServers: iceServers)
        guard rtc.isReady else {
            NSLog("acceptCall: RTCPeerConnection allocation failed")
            endLocal(reason: .error)
            return
        }
        installDelegate(on: rtc)
        CallAudioSession.configureCallOutput(loudspeakerByDefault: withVideo)
        CallAudioSession.describe("accept: before local audio track")
        // Mic on by default for the answerer, unless a mute was chosen
        // before the capture (the CallKit mute while ringing): what the
        // other side is told is what is sent.
        rtc.addLocalAudioTrack(enabled: micEnabled)

        // G3-1-video-ui — symmetric video wire-up on the
        // accept path. Same pre-SDP-build ordering rationale
        // as `completeDial`.
        if withVideo {
            startLocalCameraCapture(on: rtc)
        }

        // G3-sframe-native N-6 — same attach point as dial().
        if !attachSFrame(on: rtc) {
            NSLog("acceptCall: SFrame attach failed — refusing per security invariant #4")
            endLocal(reason: .error)
            return
        }
        self.rtc = rtc

        state = .connecting
        Task { [weak self] in
            await self?.composeAndShipAnswer(rtc: rtc, offerSdp: offerSdp)
        }
    }

    private func composeAndShipAnswer(
        rtc: WebRTCPeerConnection,
        offerSdp: String
    ) async {
        do {
            let answerSdp = try await rtc.createAnswer(remoteOfferSdp: offerSdp)
            // Ended while the answer was built: nothing is sent for a call
            // that is over, and the state the teardown reset stays reset.
            guard self.rtc === rtc else { return }
            // The senders' codecs are negotiated now: their SFrame encryptors
            // need them to leave the right codec header in the clear
            // (ADR-0071). Until told, a video encryptor keeps nothing clear.
            rtc.syncSenderCodecs()
            // `createAnswer` set the remote offer: candidates held while the
            // call rang can go in now.
            await applyBufferedRemoteCandidates(to: rtc)
            guard self.rtc === rtc else { return }
            let bytes = try composeCallSignaling(
                body: .answer(
                    callId: callId,
                    sdp: answerSdp
                )
            )
            onOutboundSignaling(
                bytes, CallSignalingWake.sessionAsks(.answer, everConnected: everConnected))
            // Arm the mid-call renegotiation drive (B2b): the initial
            // answer is sent + our local description is set, so the
            // initial negotiation is applied on the answerer side.
            negotiationReady = true
            // Don't synthesise `.connected` here — wait for
            // the RTCPeerConnection's real connectionState
            // transition. The delegate hook below drives it.
        } catch {
            guard self.rtc === rtc else { return }
            NSLog("acceptCall answer-build failed: %@", String(describing: error))
            endLocal(reason: .error)
        }
    }

    /// Apply, in arrival order, the remote candidates held until the remote
    /// description was set.
    private func applyBufferedRemoteCandidates(to rtc: WebRTCPeerConnection) async {
        let held = remoteCandidates.markRemoteDescriptionApplied()
        for candidate in held {
            await rtc.addRemoteCandidate(
                sdpMid: candidate.sdpMid,
                sdpMLineIndex: candidate.sdpMLineIndex,
                candidate: candidate.candidate
            )
        }
        if remoteCandidates.dropped > 0 {
            NSLog("CallSession: %d remote ICE candidate(s) over the buffer cap were dropped", remoteCandidates.dropped)
        }
    }

    /// Inbound side: decline a ringing call.
    public func declineCall() {
        endLocal(reason: .declined)
    }

    // MARK: - Inbound

    /// Route a parsed signaling payload into the state
    /// machine. Called from `ChatRepository.callSignalingSink`
    /// once the chat-route owner has filtered to this
    /// session's `call_id`.
    public func applyIncomingSignaling(_ body: FfiCallSignalingBody) {
        switch body {
        case .offer(let cid, let sdp, _, _):
            guard cid == callId else { return }
            if state == .idle {
                // Fresh inbound call — we're the answerer.
                isOfferer = false
                pendingRemoteOfferSdp = sdp
                state = .ringing
                return
            }
            // Mid-call renegotiation offer (the peer turned its
            // camera on, piece B2b) — NOT a fresh call. Apply with
            // the perfect-negotiation glare guard + answer, keeping
            // the call up. Only meaningful once the live peer
            // connection exists (an active call).
            guard rtc != nil, state == .connecting || state == .connected else {
                return
            }
            Task { [weak self] in await self?.applyRenegotiationOffer(sdp) }
        case .answer(let cid, let sdp):
            guard cid == callId else { return }
            guard let rtc = rtc else { return }
            if state == .outgoing {
                // Initial answer to our dial. Apply the remote
                // answer asynchronously; the real `.connected`
                // transition fires from the RTCPeerConnection state
                // delegate.
                state = .connecting
                Task { [weak self] in
                    do {
                        try await rtc.applyRemoteAnswer(sdp)
                        // Ended while the answer was applied: the buffer
                        // and the negotiation state the teardown reset stay
                        // reset.
                        guard let self, self.rtc === rtc else { return }
                        // Negotiated: tell the encryptors their codecs
                        // (ADR-0071).
                        rtc.syncSenderCodecs()
                        await self.applyBufferedRemoteCandidates(to: rtc)
                        guard self.rtc === rtc else { return }
                        // Arm the mid-call renegotiation drive now the
                        // initial exchange is applied on our side (B2b).
                        self.negotiationReady = true
                    } catch {
                        // An apply that failed because the call ended
                        // meanwhile is not an error of the call.
                        guard let self, self.rtc === rtc else { return }
                        NSLog(
                            "applyRemoteAnswer failed: %@",
                            String(describing: error)
                        )
                        self.endLocal(reason: .error)
                    }
                }
            } else {
                // Mid-call renegotiation answer (to a reneg offer we
                // sent). `isSettingRemoteAnswerPending` bridges the
                // apply window for the glare guard.
                Task { [weak self] in
                    self?.isSettingRemoteAnswerPending = true
                    do {
                        try await rtc.applyRemoteAnswer(sdp)
                        // A renegotiation (mid-call camera on) may have
                        // added a sender or changed a codec (ADR-0071).
                        rtc.syncSenderCodecs()
                    } catch {
                        NSLog(
                            "reneg applyRemoteAnswer failed: %@",
                            String(describing: error)
                        )
                    }
                    // The teardown already cleared it for a call that ended.
                    if self?.rtc === rtc { self?.isSettingRemoteAnswerPending = false }
                }
            }
        case .iceCandidate(let cid, let candidate, let sdpMid, let sdpMLineIndex):
            guard cid == callId, state != .done else { return }
            // sdpMLineIndex on the wire is UInt32; WebRTC's
            // RTCIceCandidate takes Int32. The valid range
            // for an SDP m-line index is 0..255 in practice,
            // so the cast is loss-free.
            let remote = RemoteIceCandidate(
                sdpMid: sdpMid,
                sdpMLineIndex: Int32(bitPattern: sdpMLineIndex),
                candidate: candidate
            )
            // Before the remote description is applied (ringing, or the SDP
            // still being set) the candidate is kept, not dropped; it is
            // applied in order once the description lands.
            if remoteCandidates.offer(remote) { return }
            guard let rtc = rtc else { return }
            Task { await rtc.addRemoteCandidate(
                sdpMid: remote.sdpMid,
                sdpMLineIndex: remote.sdpMLineIndex,
                candidate: remote.candidate
            ) }
        case .screenShare:
            // ADR-0083: a share state is folded only once verified against the
            // row's MLS sender (``applyScreenShare(_:callId:)``); the raw body
            // carries no verdict.
            break
        case .mediaState(let cid, let cameraOn, let microphoneOn):
            // The other person's camera and microphone (2026-11-07): the
            // mute mark and the camera-off picture on their tile.
            guard cid == callId, state != .done else { return }
            if remoteMicOn != microphoneOn { remoteMicOn = microphoneOn }
            if remoteCameraOn != cameraOn { remoteCameraOn = cameraOn }
        case .groupStart, .groupJoin, .groupLeave:
            // Group-call signaling (Phase 3c2) folds into the
            // repository's per-chat group-call record
            // (`ChatRepository.routeCallSignaling` via
            // `CallSignalingRoute`, C-11), never into the 1×1
            // `CallSession`. It cannot reach this method; the
            // exhaustive-switch contract requires naming the
            // cases.
            break
        case .end(let cid, let reason):
            guard cid == callId, state != .done else { return }
            endRemote(reason: Self.endReason(from: reason))
        }
    }

    // MARK: - Internal

    private func endLocal(reason: WebCallEndReason) {
        guard state != .done else { return }
        // A call that ends itself looks, from the other end and from the
        // transcript, exactly like one nobody answered. The reason and the
        // state it ended from are the only things that tell them apart.
        NSLog(
            "CallSession: ending locally, reason %@, from state %@",
            String(describing: reason), String(describing: state))
        state = .ending
        endReason = reason
        do {
            let bytes = try composeCallSignaling(
                body: .end(
                    callId: callId,
                    reason: Self.toFfiEndReason(reason)
                )
            )
            // An end stops a ring: the caller cancelling, the callee declining
            // or missing it. Once the call has connected nobody is ringing, and
            // a wake would ring the other person's phone for a call that is
            // over.
            onOutboundSignaling(
                bytes, CallSignalingWake.sessionAsks(.end, everConnected: everConnected))
        } catch {
            NSLog("endLocal composeCallSignaling failed: %@", error.localizedDescription)
        }
        teardownMediaPipeline()
        pendingRemoteOfferSdp = nil
        state = .done
    }

    private func endRemote(reason: WebCallEndReason) {
        guard state != .done else { return }
        endReason = reason
        teardownMediaPipeline()
        pendingRemoteOfferSdp = nil
        state = .done
    }

    /// Drive a client-initiated renegotiation offer (piece B2b: a
    /// camera track was added mid-call by `addLocalVideo`). Mirror
    /// of the web `onNegotiationNeeded`: only start an offer from a
    /// clean `.stable` state with no in-flight offer, and only
    /// after the initial exchange (`negotiationReady`). The common
    /// audio-only-then-add-video case is always `.stable`, so it is
    /// covered; a rare simultaneous glare is resolved by the peer's
    /// polite rollback in `applyRenegotiationOffer`. `makingOffer`
    /// is true only while the offer is built + shipped (MDN
    /// perfect-negotiation shape); the pending video m-line then
    /// lives in the `have-local-offer` signaling state until the
    /// answer drains it.
    /// - Parameter video: what the re-offer actually carries. It used to be
    ///   hardcoded `true`, which was right while the only renegotiation was a
    ///   camera turning on; a camera turning off now drives one too, and an
    ///   offer claiming video while withdrawing it is the kind of lie
    ///   `docs/ux/calls.md` section 1 rules out.
    private func driveRenegotiationOffer(
        rtc: WebRTCPeerConnection, video: Bool = true
    ) async {
        guard negotiationReady, !makingOffer,
            rtc.signalingState == .stable else { return }
        makingOffer = true
        do {
            let offerSdp = try await rtc.createRenegotiationOffer()
            // Ended while the offer was built: nothing follows the End.
            guard self.rtc === rtc else { return }
            let bytes = try composeCallSignaling(
                body: .offer(
                    callId: callId,
                    sdp: offerSdp,
                    createdAtMs: UInt64(Date().timeIntervalSince1970 * 1000),
                    media: FfiCallMediaFlags(audio: dialAudio, video: video)
                )
            )
            // A renegotiation (camera on or off, the peer's screen share) is an
            // offer for a call both sides are already in: no wake, or the
            // other phone rings a second, phantom call.
            onOutboundSignaling(
                bytes,
                CallSignalingWake.sessionAsks(.renegotiationOffer, everConnected: everConnected))
        } catch {
            NSLog(
                "driveRenegotiationOffer failed: %@",
                String(describing: error)
            )
        }
        if self.rtc === rtc { makingOffer = false }
    }

    /// Apply a mid-call renegotiation offer from the peer (piece
    /// B2b — the peer turned its camera on). Canonical perfect-
    /// negotiation glare guard: the OFFERER (caller) is impolite,
    /// the ANSWERER (callee) is polite (`polite = !isOfferer`). On
    /// a collision an impolite peer DROPS the offer; a polite peer
    /// YIELDS — since native WebRTC has no implicit rollback (unlike
    /// the browser), it rolls its own in-flight offer back to
    /// `.stable` explicitly, then applies + answers the remote
    /// offer.
    private func applyRenegotiationOffer(_ sdp: String) async {
        guard let rtc = rtc else { return }
        let polite = !isOfferer
        let readyForOffer = !makingOffer
            && (rtc.signalingState == .stable || isSettingRemoteAnswerPending)
        let ignoreOffer = !polite && !readyForOffer
        if ignoreOffer { return }
        do {
            // Explicit rollback of our own in-flight offer (glare):
            // `createAnswer` sets the remote offer, which is illegal
            // from `have-local-offer` — roll back to `.stable` first.
            if rtc.signalingState != .stable {
                try await rtc.rollbackLocalDescription()
            }
            let answerSdp = try await rtc.createAnswer(remoteOfferSdp: sdp)
            // Ended while the answer was built: nothing follows the End.
            guard self.rtc === rtc else { return }
            // The renegotiation is applied on this side (ADR-0071).
            rtc.syncSenderCodecs()
            let bytes = try composeCallSignaling(
                body: .answer(callId: callId, sdp: answerSdp)
            )
            onOutboundSignaling(
                bytes, CallSignalingWake.sessionAsks(.answer, everConnected: everConnected))
        } catch {
            NSLog(
                "applyRenegotiationOffer failed: %@",
                String(describing: error)
            )
        }
    }

    /// G3-1-video-ui — common teardown for the WebRTC pod
    /// + camera capture. Called from both `endLocal` and
    /// `endRemote` so a hung-up or remote-end-of-call
    /// path releases the camera + clears the SwiftUI
    /// preview overlay identically.
    private func teardownMediaPipeline() {
        // G3-1-conn-quality — stop the stats poll loop before
        // dropping the peer connection.
        statsTask?.cancel()
        statsTask = nil
        for line in inboundCounters.total() {
            NSLog("%@", line)
        }
        rtc?.close()
        rtc = nil
        if let capture = self.localCameraCapture {
            // Fire-and-forget — `LocalCameraCapture.deinit`
            // is a safety-net stop, so even if this Task is
            // cancelled before completion the AVCaptureSession
            // gets released.
            Task { await capture.stop() }
            self.localCameraCapture = nil
        }
        self.localVideoTrack = nil
        // G3-1-remote-video — drop the remote handle so the
        // gallery tile reverts to the avatar placeholder on
        // both end paths. Detaching the frame probe with it keeps
        // `remoteVideoFlowing` from surviving into the next call.
        self.remoteVideoTrack = nil
        self.watchRemoteVideoFrames(on: nil)
        // No share outlives the call.
        self.setRemoteScreenTrack(nil)
        self.screenShare = nil
        self.screenShares = ScreenShareFold(ownKey: Data(count: 32))
        // The other side's camera and microphone state is the call's.
        self.mediaAnnouncement.reset()
        if !self.remoteMicOn { self.remoteMicOn = true }
        if !self.remoteCameraOn { self.remoteCameraOn = true }
        // G3-1-conn-quality — clear the stale stats so a fresh
        // call doesn't briefly show the previous RTT / bitrate.
        self.rttMs = nil
        self.connectionRelayed = false
        self.downBps = nil
        self.upBps = nil
        self.prevStatsBytes = nil
        self.inboundActivity = nil
        self.callPathLog = CallPathLog()
        // Reset perfect-negotiation state (B2b) so a late
        // renegotiation tick after teardown no-ops.
        self.negotiationReady = false
        self.makingOffer = false
        self.isSettingRemoteAnswerPending = false
        self.remoteCandidates.reset()
        self.addingVideo = false
        // B2a — a fresh call starts with no camera-on in flight.
        setVideoPublishState(.off)
        // The next call's microphone depends on what this one leaves behind
        // (see `CallAudioSession`), and nothing else reports it.
        CallAudioSession.describe("teardown")
    }

    /// G3-1-conn-quality — start the connection-stats poll loop
    /// once, on the first `.connected` transition. Samples the
    /// pod's `RTCStatisticsReport` every 2 s and republishes
    /// `rttMs` / `connectionRelayed` on the main actor. The loop
    /// ends when the task is cancelled (`teardownMediaPipeline`)
    /// or the `Rtc` goes away.
    private func startStatsPollingIfNeeded() {
        guard statsTask == nil else { return }
        statsTask = Task { [weak self] in
            let startedAtSec = ProcessInfo.processInfo.systemUptime
            while !Task.isCancelled {
                guard let self, let rtc = self.rtc else { return }
                if let snap = await rtc.sampleConnectionStats() {
                    // The call may have ended while the report was read: a
                    // late sample must not put statistics back on a call
                    // whose teardown just cleared them.
                    guard !Task.isCancelled, self.rtc === rtc else { return }
                    // iter2 — derive bitrate from the byte delta against
                    // a monotonic clock (robust to poll-loop jitter; the
                    // 2 s sleep is only a nominal cadence).
                    let nowSec = ProcessInfo.processInfo.systemUptime
                    let prev = self.prevStatsBytes
                    var down: Int?
                    var up: Int?
                    if snap.hasActivePair, let p = prev {
                        let dt = nowSec - p.atSec
                        if dt > 0.1 {
                            let dRx = max(0, snap.bytesReceived - p.received)
                            let dTx = max(0, snap.bytesSent - p.sent)
                            down = Int((Double(dRx) * 8.0 / dt).rounded())
                            up = Int((Double(dTx) * 8.0 / dt).rounded())
                        }
                    }
                    self.prevStatsBytes = snap.hasActivePair
                        ? (received: snap.bytesReceived, sent: snap.bytesSent, atSec: nowSec)
                        : nil
                    self.inboundActivity = MediaWatchdog.note(
                        self.inboundActivity,
                        packets: snap.mediaPacketsReceived,
                        atSec: nowSec)
                    // Say the flow once, a few samples into a connected call.
                    // "Nothing is arriving" is half the question; whether this
                    // device was SENDING is the other half, and only the pair
                    // names the direction that is dead. The web side logs the
                    // same two numbers, so either end can blame a direction
                    // on its own.
                    if !self.loggedMediaFlow, nowSec - startedAtSec > 8 {
                        self.loggedMediaFlow = true
                        let signalled = Self.signalledSsrc(
                            sdp: self.rtc?.localDescriptionSdp ?? "")
                        NSLog(
                            "CallSession: media flow rtpIn=%lldpkt rtpOut=%lldpkt "
                                + "ssrcSending=%lld ssrcSignalled=%@",
                            snap.mediaPacketsReceived, snap.mediaPacketsSent,
                            snap.mediaSsrcSent, signalled)
                    }
                    // The receivers' counters, from a second read of the
                    // report: the snapshot above carries numbers only.
                    if let entries = await rtc.sampleMediaStatEntries(),
                        !Task.isCancelled, self.rtc === rtc {
                        for line in self.inboundCounters.interval(entries) {
                            NSLog("%@", line)
                        }
                    }
                    if let entries = await rtc.samplePathStatEntries(),
                        !Task.isCancelled, self.rtc === rtc,
                        let line = self.callPathLog.note(entries) {
                        NSLog("%@", line)
                    }
                    await MainActor.run { self.endIfMediaHasGoneSilent(atSec: nowSec) }
                    await MainActor.run {
                        self.rttMs = snap.rttMs
                        self.connectionRelayed = snap.relayed
                        self.downBps = down
                        self.upBps = up
                    }
                }
                try? await Task.sleep(nanoseconds: 2_000_000_000)
            }
        }
    }

    // MARK: - WebRTC delegate plumbing

    /// Attach (or detach) the first-frame probe for the remote video track.
    ///
    /// Re-attaching on every track change matters: a peer that stops and
    /// restarts its camera produces a NEW track, and a stale `true` would send
    /// the screen to the video layout before the new track has drawn anything.
    // MARK: - Screen share (ADR-0083 section 4)

    /// Fold a share state already verified against the row's MLS sender leaf
    /// (``ChatRepository/routeCallSignaling``). A state for another call is
    /// dropped.
    func applyScreenShare(_ verified: VerifiedCallScreenShare, callId: Data) {
        guard callId == self.callId, state != .done else { return }
        if screenShares.observe(verified) {
            NSLog(
                "CallSession: current screen share changed: live=%@",
                screenShares.current != nil ? "true" : "false")
            screenShare = screenShares.current
        }
    }

    /// Keep (or, `nil`, drop) the other person's screen track, and probe it
    /// for its first frame.
    private func setRemoteScreenTrack(_ track: RTCVideoTrack?) {
        if let previous = remoteScreenTrack, let probe = remoteScreenProbe {
            previous.remove(probe)
        }
        remoteScreenProbe = nil
        remoteScreenFlowing = false
        remoteScreenTrack = track
        guard let track else { return }
        NSLog("CallSession: remote screen track attached; waiting for the first frame")
        let probe = RemoteVideoFrameProbe { [weak self, weak track] in
            guard let self, let track, self.remoteScreenTrack === track else { return }
            NSLog("CallSession: remote screen first frame rendered")
            self.remoteScreenFlowing = true
        }
        track.add(probe)
        remoteScreenProbe = probe
    }

    private func watchRemoteVideoFrames(on track: RTCVideoTrack?) {
        if let previous = probedRemoteVideoTrack, let probe = remoteVideoProbe {
            previous.remove(probe)
        }
        remoteVideoProbe = nil
        probedRemoteVideoTrack = nil
        remoteVideoFlowing = false
        remoteVideoDiagnosisTask?.cancel()
        remoteVideoDiagnosisTask = nil
        guard let track else { return }
        // The two log lines that separate the two ways a remote video can fail.
        // "track" without "first frame" means the peer announced a camera and
        // nothing openable arrived — the SDP landed, so the break is in the
        // media or in SFrame, not in the signaling. Both together mean the
        // picture is up. Plain text, no payload: a track id is not ours to log.
        NSLog("CallSession: remote video track attached; waiting for the first frame")
        scheduleRemoteVideoDiagnosis()
        let probe = RemoteVideoFrameProbe { [weak self, weak track] in
            // Only the track this probe was attached to may flip the flag: a
            // late frame from a track that has since been replaced must not
            // claim the new one is flowing.
            guard let self, let track, self.probedRemoteVideoTrack === track else { return }
            NSLog("CallSession: remote video first frame rendered")
            self.remoteVideoFlowing = true
            // The other person is on screen: the call plays on the
            // loudspeaker unless a headset or a Bluetooth device is connected.
            CallAudioSession.callGainedVideo()
        }
        track.add(probe)
        remoteVideoProbe = probe
        probedRemoteVideoTrack = track
    }

    /// How long after a remote video track attaches to read the inbound
    /// counters. Matches the cadence the Android client already logs at, so the
    /// two sides of one call are directly comparable in a transcript.
    private static let remoteVideoDiagnosisDelaySeconds = 4
    /// How long after the FIRST packet to take the late reading — enough for a
    /// keyframe and its assembly, short enough to stay in the same breath as the
    /// sender's own log line.
    private static let remoteVideoSettleSeconds = 4
    /// The ticker's period.
    private static let remoteVideoTickSeconds = 1
    /// How long to keep looking, from the track attaching. Sized off a live run
    /// where the sender's first packet came **nine seconds** after the track
    /// attached, so a budget near that would expire before the interesting
    /// moment; this leaves room for a considerably slower renegotiation.
    private static let remoteVideoBudgetSeconds = 45

    /// Read the inbound video counters twice and log both, labelled.
    ///
    /// ## Why not once
    ///
    /// The first version fired once, four seconds after the track attached — and
    /// a track attaches when the SDP is applied, which on a live call was nine
    /// seconds before the sender's first packet. It reported `NO_PACKETS` about
    /// a moment when no packet could exist, and never looked again: true, and
    /// worse than useless, because it invites a conclusion.
    ///
    /// So the early reading is kept (it is the one that says whether anything
    /// arrived immediately) and a **late** one is added, anchored to an event on
    /// the wire rather than a local clock: a short ticker watches for the first
    /// packet, then reads again once the sender is known to be flowing. Frames
    /// decoding ends it early; a budget guarantees a line either way, because a
    /// probe that goes quiet reads exactly like one that never ran.
    ///
    /// ``RemoteVideoDiagnosis/step(stats:rendered:secondsSinceFirstPacket:secondsRemaining:settleSeconds:)``
    /// is the whole rule; this method only supplies clocks and I/O.
    private func scheduleRemoteVideoDiagnosis() {
        remoteVideoDiagnosisTask?.cancel()
        remoteVideoDiagnosisTask = Task { @MainActor [weak self] in
            await self?.runRemoteVideoDiagnosis()
        }
    }

    /// The probe body. Split out so the task closure stays a one-liner.
    private func runRemoteVideoDiagnosis() async {
        let early = Self.remoteVideoDiagnosisDelaySeconds
        try? await Task.sleep(nanoseconds: UInt64(early) * 1_000_000_000)
        guard let first = await sampleRemoteVideoIfLive() else { return }
        emitRemoteVideoReading(first.stats, rendered: first.rendered,
                               reading: .early(afterSeconds: early))

        let startedAt = Date()
        var firstPacketAt: Date?
        while !Task.isCancelled {
            try? await Task.sleep(
                nanoseconds: UInt64(Self.remoteVideoTickSeconds) * 1_000_000_000)
            guard let sample = await sampleRemoteVideoIfLive() else { return }
            if firstPacketAt == nil, sample.stats.packetsReceived > 0 {
                firstPacketAt = Date()
            }
            let elapsed = Int(Date().timeIntervalSince(startedAt))
            let step = RemoteVideoDiagnosis.step(
                stats: sample.stats,
                rendered: sample.rendered,
                secondsSinceFirstPacket: firstPacketAt.map {
                    Int(Date().timeIntervalSince($0))
                },
                secondsRemaining: Self.remoteVideoBudgetSeconds - elapsed,
                settleSeconds: Self.remoteVideoSettleSeconds)
            guard case .report(let reading) = step else { continue }
            // The budget case reports how long it actually waited, which the
            // pure rule cannot know.
            let labelled: RemoteVideoReading = {
                if case .budgetExpired = reading {
                    return .budgetExpired(afterSeconds: elapsed + early)
                }
                return reading
            }()
            emitRemoteVideoReading(sample.stats, rendered: sample.rendered,
                                   reading: labelled)
            return
        }
    }

    /// One sample, or `nil` when the call or the track has gone away.
    private func sampleRemoteVideoIfLive() async -> (stats: InboundVideoStats, rendered: Bool)? {
        guard !Task.isCancelled, let rtc = self.rtc,
            self.probedRemoteVideoTrack != nil
        else { return nil }
        return (await rtc.sampleInboundVideoStats(), self.remoteVideoFlowing)
    }

    private func emitRemoteVideoReading(
        _ stats: InboundVideoStats, rendered: Bool, reading: RemoteVideoReading
    ) {
        let verdict = RemoteVideoDiagnosis.diagnose(stats, rendered: rendered)
        NSLog("%@", verdict.logLine(stats, rendered: rendered, reading: reading))
    }

    /// Wire the wrapper's typed callbacks back into the
    /// session. `self` is captured weakly to defend against
    /// the wrapper outliving the session in error-path
    /// teardown.
    private func installDelegate(on rtc: WebRTCPeerConnection) {
        rtc.installDelegate(.init(
            onLocalCandidate: { [weak self] sdpMid, mLineIndex, sdp in
                self?.shipLocalCandidate(
                    sdpMid: sdpMid,
                    sdpMLineIndex: mLineIndex,
                    candidate: sdp
                )
            },
            onConnectionState: { [weak self] newState in
                self?.handleConnectionState(newState)
            },
            onRemoteVideoTrack: { [weak self] track, streamId in
                guard let self else { return }
                // ADR-0083: a stream id ending in `~screen` is the other
                // person's shared screen, not their camera.
                if case .screen = RemoteVideoRoute.of(streamId: streamId) {
                    self.setRemoteScreenTrack(track)
                    return
                }
                // A removal names no stream: it is the screen's when the
                // track the pod just removed is the screen track.
                if track == nil, let screen = self.remoteScreenTrack,
                    self.lastRemovedVideoTrackId == screen.trackId {
                    self.setRemoteScreenTrack(nil)
                    return
                }
                // 1×1: a single remote camera track.
                self.remoteVideoTrack = track
                self.watchRemoteVideoFrames(on: track)
            },
            onRemoteReceiver: { [weak self] receiver, _ in
                // Arm the SFrame decryptor on EVERY incoming track
                // (audio + video) using the per-call seed injected via
                // `setCallKey` — the 1×1 media is SFrame-sealed
                // (invariant #4), so the receive side must open what the
                // peer's encryptors sealed. Fires for the initial audio
                // (+ any initial video) track at connect AND for a
                // mid-call video track the peer adds (piece B2b), so the
                // peer's mid-call camera-on decrypts without extra
                // wiring. A nil seed (setCallKey never ran → the call
                // refuses at attach) is a safe skip.
                guard let self, let rtc = self.rtc,
                    let seed = self.callKeySeed else { return }
                let kind = receiver.track?.kind ?? "unknown"
                let armed = rtc.armSFrameDecryptor(for: receiver, seed: seed)
                // The result used to be discarded, which made a failed attach on
                // a mid-call receiver completely silent — and a receiver with no
                // decryptor drops every sealed frame, which looks from the
                // outside exactly like "the peer is not sending". Whatever else
                // is true, this is now on the record.
                NSLog(
                    "CallSession: SFrame decryptor arm: kind=%@ ok=%@",
                    kind, armed ? "true" : "false")
                // ...and on the screen. The call is NOT ended on a failure:
                // arming is per receiver, so audio can be fine while video is
                // dead, and hanging up would replace a describable state with a
                // dropped call — the same ambiguity one layer further out.
                if !armed { self.remoteMediaUndecryptable = true }
            },
            onRemoteVideoTrackRemoved: { [weak self] trackId in
                self?.lastRemovedVideoTrackId = trackId
            }
        ))
    }

    private func shipLocalCandidate(
        sdpMid: String,
        sdpMLineIndex: Int32,
        candidate: String
    ) {
        // WebRTC `Int32` → wire `UInt32`. Symmetric to the
        // applyIncomingSignaling cast above.
        let wireIndex = UInt32(bitPattern: sdpMLineIndex)
        do {
            let bytes = try composeCallSignaling(
                body: .iceCandidate(
                    callId: callId,
                    candidate: candidate,
                    sdpMid: sdpMid,
                    sdpMLineIndex: wireIndex
                )
            )
            onOutboundSignaling(
                bytes, CallSignalingWake.sessionAsks(.candidate, everConnected: everConnected))
        } catch {
            NSLog(
                "shipLocalCandidate composeCallSignaling failed: %@",
                error.localizedDescription
            )
        }
    }

    /// Map RTCPeerConnectionState transitions to the public
    /// WebCallState. Only `.connected`, `.disconnected`,
    /// `.failed`, `.closed` are actionable — `.new` /
    /// `.connecting` are transient.
    private func handleConnectionState(_ newState: RTCPeerConnectionState) {
        lastConnectionState = newState
        switch newState {
        case .connected:
            // First `.connected` after connecting → move to
            // the public connected state. Reconnects after a
            // brief `.disconnected` interlude also pass
            // through here, which is what cancels the grace.
            cancelIceGrace()
            if state == .connecting || state == .outgoing {
                everConnected = true
                state = .connected
                // The first camera and microphone state of the call.
                announceMediaStateIfChanged()
            }
            // G3-1-conn-quality — begin polling connection stats
            // once the transport is up. Idempotent: a flap back
            // through `.connected` won't spawn a second loop.
            startStatsPollingIfNeeded()
        case .failed:
            cancelIceGrace()
            endLocal(reason: .error)
        case .disconnected:
            // WebRTC sometimes flaps disconnected → connected on a transient
            // network glitch (wifi → LTE handover), so this is a grace and
            // not an ending: a recovery through `.connected` cancels it.
            //
            // This used to do nothing at all, on the ground that "the
            // upper-layer watchdog will fire `.timedOut`". It does not: the
            // host's establish timer matches only `.outgoing` / `.connecting`
            // / an accepted `.ringing`, so a CONNECTED call whose transport
            // had gone was never ended by anything.
            armIceGrace()
            break
        case .closed:
            cancelIceGrace()
            if state != .done {
                endRemote(reason: .hungUp)
            }
        case .new, .connecting:
            break
        @unknown default:
            break
        }
    }

    /// Arm the ICE-`disconnected` grace. Idempotent: a second `.disconnected`
    /// while one is already running keeps the first window rather than
    /// extending it, so a flapping transport cannot postpone the ending
    /// indefinitely.
    private func armIceGrace() {
        guard iceGraceTask == nil, state == .connecting || state == .connected else { return }
        iceGraceTask = Task { [weak self] in
            try? await Task.sleep(
                nanoseconds: UInt64(Self.iceDisconnectedGraceSeconds * 1_000_000_000))
            guard !Task.isCancelled else { return }
            await MainActor.run {
                guard let self else { return }
                self.iceGraceTask = nil
                switch self.lastConnectionState {
                case .disconnected, .failed:
                    self.endLocal(reason: .error)
                default:
                    break
                }
            }
        }
    }

    private func cancelIceGrace() {
        iceGraceTask?.cancel()
        iceGraceTask = nil
    }

    /// End a connected call that has received nothing for
    /// ``mediaSilenceTimeoutSeconds`` (`MediaWatchdog`).
    ///
    /// Only while `.connected`: a call still being set up has not received
    /// anything YET, which is a different thing from having stopped.
    private func endIfMediaHasGoneSilent(atSec: TimeInterval) {
        guard state == .connected else { return }
        guard
            MediaWatchdog.hasGoneSilent(
                inboundActivity, atSec: atSec, timeoutSec: Self.mediaSilenceTimeoutSeconds)
        else { return }
        NSLog(
            "CallSession: nothing received for %.0f s; ending the call",
            Self.mediaSilenceTimeoutSeconds)
        endLocal(reason: .error)
    }

    private static func endReason(from ffi: FfiCallEndReason) -> WebCallEndReason {
        switch ffi {
        case .hungUp: return .hungUp
        case .busy: return .busy
        case .declined: return .declined
        case .timedOut: return .timedOut
        case .error: return .error
        // A group / link reason (`docs/ux/calls.md` section 4.1.2). A 1x1
        // call has no such choice: ending it ends it for both, so here it is
        // a plain hang-up.
        case .endedForAll: return .hungUp
        }
    }

    private static func toFfiEndReason(_ r: WebCallEndReason) -> FfiCallEndReason {
        switch r {
        case .hungUp: return .hungUp
        case .busy: return .busy
        case .declined: return .declined
        case .timedOut: return .timedOut
        case .error: return .error
        }
    }
}

// MARK: - Errors

/// Errors the 1×1 `CallSession` surfaces to the caller (mid-call
/// camera-on, piece B2b). A camera-start failure rethrows the
/// underlying `LocalCameraCapture.CaptureError` instead.
public enum CallSessionError: Error, CustomStringConvertible {
    /// The host has no usable camera (gate on
    /// `LocalCameraCapture.hasUsableCamera()` before calling
    /// `addLocalVideo`).
    case noCamera
    /// The mid-call video sender could not be SFrame-sealed
    /// (missing per-call seed / leaf, calling unavailable, or the
    /// attach failed). Fail-closed per invariant #4 — the call
    /// stays up audio-only rather than offering an unsealed video
    /// m-line.
    case sframeUnavailable

    public var description: String {
        switch self {
        case .noCamera:
            return "No camera is available on this device"
        case .sframeUnavailable:
            return "The camera could not be end-to-end encrypted"
        }
    }
}

// MARK: - call_id extraction helper

extension CallSession {
    /// Pull the embedded `call_id` from any
    /// [`FfiCallSignalingBody`] variant. The chat-route
    /// dispatcher uses this to route stale signaling away
    /// from the active session.
    public static func callId(of body: FfiCallSignalingBody) -> Data {
        switch body {
        case .offer(let cid, _, _, _),
             .answer(let cid, _),
             .iceCandidate(let cid, _, _, _),
             .end(let cid, _):
            return cid
        case .groupStart(let cid, _, _, _),
             .groupJoin(let cid, _, _),
             .groupLeave(let cid, _, _):
            return cid
        case .screenShare(let cid, _):
            return cid
        case .mediaState(let cid, _, _):
            return cid
        }
    }
}

/// When a one-to-one call tells the other side its camera and microphone
/// state (the `mediaState` call signal, 2026-11-07; mirror of the web
/// `CallSession.announceMediaStateIfChanged`): nothing before the call is
/// connected, the first state when it connects, and after that each state
/// that differs from the last one told. Pure, so the rule is testable without
/// a peer connection.
struct OneToOneMediaAnnouncement: Equatable {
    struct Media: Equatable {
        let camera: Bool
        let microphone: Bool
    }

    /// The state last told to the other side; `nil` until the first one.
    private(set) var announced: Media?

    /// The state to send now, recorded as told, or `nil` when there is
    /// nothing to say (not connected, or unchanged).
    mutating func next(connected: Bool, camera: Bool, microphone: Bool) -> Media? {
        guard connected else { return nil }
        let media = Media(camera: camera, microphone: microphone)
        if announced == media { return nil }
        announced = media
        return media
    }

    /// The call ended: the next call starts from nothing told.
    mutating func reset() {
        announced = nil
    }
}
