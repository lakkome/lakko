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
/// camera (`LocalCameraCapture`), adds its senders BEFORE arming SFrame +
/// building the offer (so the offer advertises their m-lines and each is
/// SFrame-sealed alongside audio), and surfaces the local track through
/// `onLocalVideoTrack` for the self-preview tile.
///
/// The camera goes out as three ordinary senders (ADR-0070 steps 1 and 5,
/// ``CameraSender``): the main one, a light one at a quarter of the size and
/// a middle one at half the size, each sealed under its own SFrame stream
/// index; the offer names the light and the middle m-line to the SFU. Which
/// of them encode and send now is ``SenderActivityRule`` over the people in
/// the call and the SFU's `video-needed` word. What this device asks the SFU
/// for of each other person is ``VideoWantsBook``, fed by the tiles; what
/// the SFU says it gives is ``VideoGivenBook``.
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
        /// The session was hung up, failed, or was told by the SFU that the
        /// call had ended while a step of the connect was awaited. Nothing
        /// was captured or published for it.
        case endedWhileConnecting

        var errorDescription: String? {
            switch self {
            case .endedWhileConnecting:
                return "The call ended before it connected."
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
        /// ADR-0078 section 2: composes this device's signed peer announcement
        /// and verifies everyone else's against the call's MLS roster. The
        /// session seals it under the call seed's relay key and broadcasts it
        /// (after the Welcome, after every rekey, when someone joins, and every
        /// ``SfuCallSession/peerAnnounceIntervalMs``); a verified one fires
        /// ``Callbacks/onPeerIdentified``. `nil`: nobody is named (tests).
        var peerAnnouncer: CallPeerAnnouncer? = nil
        /// The call id, lower-case hex: the key a name typed for this call is
        /// remembered under for a rejoin in this app run (``CallNameMemory``).
        /// `nil`: nothing is remembered (tests).
        var callIdHex: String? = nil
    }

    /// How often a device re-announces who it is to the call (ADR-0078
    /// section 2). Chosen, not measured: an announcement sealed under a relay
    /// key a peer did not hold yet is lost to that peer, and the next one
    /// names the tile. Same value as web and Android.
    static let peerAnnounceIntervalMs: Int64 = 15_000

    struct Callbacks {
        var onStateChange: ((State) -> Void)?
        var onWelcome: ((_ selfPeerId: String, _ roster: [SfuPeerSnapshot]) -> Void)?
        /// A participant joined. The SFU says nothing about who it is;
        /// ``onPeerIdentified`` does, once its announcement verifies.
        var onPeerJoined: ((_ peerId: String) -> Void)?
        /// A peer's sealed announcement verified against the call's roster
        /// (ADR-0078 section 2). Fired again on every re-announcement.
        /// Defaulted so existing memberwise callers keep their initialiser.
        var onPeerIdentified: ((_ peerId: String, _ identity: VerifiedPeerIdentity) -> Void)? = nil
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
        /// A remote peer said whether its microphone is on (`false` = muted),
        /// in the same sealed media-state envelope as the camera
        /// (`lakko_contacts::media_state` version 2). Fired with every opened
        /// media state, after ``onRemoteMediaState``. `from` is the sender's
        /// SFU peer id. Drives the mute mark on that person's tile
        /// (`docs/ux/group-call-grid.md` 7.2). Defaulted so existing
        /// memberwise callers keep their shorter initialiser.
        var onRemoteMicrophoneState: ((_ from: String, _ microphoneOn: Bool) -> Void)? = nil
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
        /// The light companion of `peerId`'s camera arrived (ADR-0067 step 5,
        /// ``LightStream``): a second, lighter picture of a person already in
        /// the call, delivered under that person's stream id plus `~light`.
        /// It never reaches ``onRemoteVideoTrack``, so it can never be taken
        /// for a participant's main track. Defaulted so existing memberwise
        /// callers keep their shorter initialiser.
        var onRemoteLightVideoTrack: ((RTCVideoTrack, _ peerId: String) -> Void)? = nil
        /// The middle track of `peerId`'s camera arrived (ADR-0070 step 5):
        /// the same camera at half size, delivered under that person's
        /// stream id plus `~mid`. Like the light companion it is a further
        /// picture of a person already in the call and never reaches
        /// ``onRemoteVideoTrack``. Its packets flow only while this device
        /// asks for that person's middle picture.
        var onRemoteMiddleVideoTrack: ((RTCVideoTrack, _ peerId: String) -> Void)? = nil
        /// What the SFU says it gives of each person's video now, by SFU
        /// peer id (ADR-0070 step 6): the whole set. Empty before the first
        /// `video-given` frame and with downlink control off.
        var onVideoGiven: (([String: TileVideoQuality]) -> Void)? = nil
        /// The call's current screen share changed (ADR-0083): the fold's
        /// answer after a verified share state or a sharer leaving, `nil`
        /// when no share is live.
        var onScreenShareChanged: ((CallCurrentScreenShare?) -> Void)? = nil
        /// The shared screen of `peerId` arrived (or, `nil`, went): a remote
        /// video track under `<peer>~screen`. Its own tile, never a picture
        /// of the person's camera, and never in `video-wants`.
        var onRemoteScreenVideoTrack: ((RTCVideoTrack?, _ peerId: String) -> Void)? = nil
        /// A peer's name for this call changed (`docs/ux/calls.md` section
        /// 4.1, ``CallNameChannel``): signed with the leaf key that peer's
        /// announcement verified with, newest per peer. `nil` when the peer
        /// went back to its default naming.
        var onRemoteCallName: ((_ peerId: String, _ name: String?) -> Void)? = nil
        /// A message in the call's chat (`docs/ux/calls.md` section 4.1.4,
        /// ``CallChatChannel``) verified against the leaf key its sender's
        /// announcement verified with; each `(peer, seq)` once.
        var onCallChat: ((_ peerId: String, _ seq: UInt64, _ text: String) -> Void)? = nil
    }

    private let config: Config
    /// Internal (not private) so a unit test can drive the adapter's
    /// callbacks without an SFU.
    let callbacks: Callbacks
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
    /// The person's microphone choice (`false` = muted), broadcast beside
    /// the camera state so the other tiles show the mute mark
    /// (`docs/ux/group-call-grid.md` 7.2). Recorded by
    /// ``setMicrophoneEnabled(_:)`` even before capture, and applied to the
    /// captured microphone, so what is announced is what is sent.
    private var localMicOn: Bool
    private var signaling: SfuSignalingClient?
    private var pc: WebRTCPeerConnection?
    /// Whose audio each receiving audio track carries: track id -> SFU peer
    /// id (the MSID stream id the SFU pins on what it forwards). Read by
    /// ``sampleAudioLevels()`` to attribute a level to a person; never sent
    /// anywhere.
    private var audioTrackPeers: [String: String] = [:]
    /// The ids of the receiving video tracks that are light companions and
    /// middle tracks, so the media log can say `layer=light` and
    /// `layer=middle` for their receivers.
    private var lightTrackIds: Set<String> = []
    private var middleTrackIds: Set<String> = []
    /// The leaf signature key each peer's verified announcement named, by SFU
    /// peer id: what a screen-share state from that peer must be signed by
    /// (ADR-0083 section 1), and whose share ends when the peer leaves.
    private var peerSignerKeys: [String: Data] = [:]
    /// The call's one-share-at-a-time fold (C-87).
    private lazy var screenShares = ScreenShareFold(
        ownKey: config.peerAnnouncer?.ownSignatureKey ?? Data())
    /// When the last media reading was taken, on a clock that never goes
    /// back; for the senders' bitrate over the interval.
    private var mediaLogLastAtMs: UInt64?
    /// Each remote person's sound and picture kept together (ADR-0070, note
    /// of 2026-10-12): one shared rule per person, fed from the receivers'
    /// statistics. Whether its answers can be written to a receiver is what
    /// ``AvReceiverDelay`` says when the session is built; where they cannot,
    /// the rule measures and logs the skew and writes nothing.
    private let avAlign = CallAvAlign()
    /// The receivers the rule's answers are written to, by the id of their
    /// track, with whose they are. Held from the moment a track arrives to
    /// the person's leaving or hang-up; an `RTCRtpReceiver` keeps its engine
    /// object alive, so a write never meets a freed one.
    private var avReceivers: [String: (peerId: String, receiver: RTCRtpReceiver)] = [:]
    /// Reads the receivers every ``AvAlignStats/readingIntervalMs(_:)`` from
    /// the moment media connects to hang-up.
    private var avAlignTask: Task<Void, Never>?

    // ---- The camera's three senders (ADR-0070) -----------------------------
    /// The SFU's last `video-needed` frame: which of this device's camera
    /// senders somebody is being sent. `nil` before the first frame, which
    /// means every sender is needed.
    private var videoNeeded: VideoNeeded?
    /// Keeps each camera sender's one encoding active or not, as
    /// ``sendersActiveNow()`` says. One per sender, so the last decision
    /// wins; its writes go the one way every write of a sender's parameters
    /// goes (``SenderParameterWrites``), so none overlaps another.
    private let senderActivity: [CameraSenderKind: SenderActivity] = [
        .main: SenderActivity(name: "main"),
        .middle: SenderActivity(name: "middle"),
        .light: SenderActivity(name: "light"),
    ]
    /// The SFU's last `publish-budget` frame (``PublishBudgetRule``): how
    /// much the main and the middle camera sender may send. `nil` before the
    /// first frame, which means each sender keeps the ceiling it was created
    /// with.
    private var publishBudget: PublishBudget?
    /// Keeps the main and the middle camera sender's one encoding under the
    /// ceiling the SFU last gave. Its writes go the one way every write of a
    /// sender's parameters goes (``SenderParameterWrites``).
    private let senderCeilings: [CameraSenderKind: SenderCeiling] = [
        .main: SenderCeiling(name: "main"),
        .middle: SenderCeiling(name: "middle"),
    ]
    private let rules: TileQualityRules

    // ---- Who this device is to the others (ADR-0078 section 2) ----
    /// The relay keys of the current and previous call epoch.
    private let relay = CallRelay()
    /// The periodic peer re-announcement.
    private var announceTask: Task<Void, Never>?
    /// This device's name for the call and the newest one per peer
    /// (``CallNameChannel``). Re-sent after every announcement.
    private lazy var callNames = CallNameChannel(deps: CallNameChannel.Deps(
        compose: { [weak self] selfPeerId, seq, name in
            self?.config.peerAnnouncer?.composeCallName(selfPeerId, seq, name)
        },
        verify: { [weak self] from, signerKey, bytes in
            self?.config.peerAnnouncer?.verifyCallName(from, signerKey, bytes)
        },
        sendSealed: { [weak self] plaintext in
            guard let self, self.signaling != nil, let sealed = self.relay.seal(plaintext) else {
                return false
            }
            self.sendSignalRelayToCall(payload: sealed)
            return true
        },
        selfPeerId: { [weak self] in self?.selfPeerId },
        onName: { [weak self] peerId, name in self?.callbacks.onRemoteCallName?(peerId, name) }
    ))
    /// The call's chat (``CallChatChannel``): this device's sequence numbers,
    /// messages held for a peer not yet named, what was already shown. Unlike
    /// the call name, a message is never re-sent on a re-announcement.
    private lazy var callChats = CallChatChannel(deps: CallChatChannel.Deps(
        compose: { [weak self] selfPeerId, seq, text in
            self?.config.peerAnnouncer?.composeCallChat(selfPeerId, seq, text)
        },
        verify: { [weak self] from, signerKey, bytes in
            self?.config.peerAnnouncer?.verifyCallChat(from, signerKey, bytes)
        },
        sendSealed: { [weak self] plaintext in
            guard let self, self.isLive, self.signaling != nil,
                  let sealed = self.relay.seal(plaintext)
            else { return false }
            self.sendSignalRelayToCall(payload: sealed)
            return true
        },
        selfPeerId: { [weak self] in self?.selfPeerId },
        onMessage: { [weak self] peerId, message in
            self?.callbacks.onCallChat?(peerId, message.seq, message.text)
        }
    ))

    // ---- What this device shows of each person (ADR-0070 steps 5 and 6) ----
    /// The SFU's id for this device, from the Welcome.
    private var selfPeerId: String?
    /// The people of the call as the SFU names them, and the picture every
    /// mounted tile reports. Sent to the SFU as a whole whenever it changes.
    private var wantsBook = VideoWantsBook()
    private lazy var wantsSender = VideoWantsSender(
        wants: { [weak self] in self?.wantsBook.wants() ?? [] },
        send: { [weak self] frame in self?.sendVideoWants(frame) ?? false })
    /// What the SFU says it gives of each person. Read by the tiles and the
    /// media log; never fed back into ``wantsBook``, so the SFU keeps being
    /// asked for what the tiles really want.
    private var givenBook = VideoGivenBook()
    /// The publish offer is out: the SFU has media state for this peer, and
    /// takes a `video-wants` frame only from then on.
    private var publishOfferSent = false
    /// Reads the video counters every ``CallMediaCounters/intervalMs`` from
    /// the moment media connects to hang-up.
    private var mediaLogTask: Task<Void, Never>?
    /// The last reading, for the next interval's difference and for the
    /// total at the end.
    private var mediaLogLast: CallMediaReading?
    /// Which network path the media takes to the SFU, logged when it is
    /// first known and when it changes (``CallPath``).
    private var callPathLog = CallPathLog()
    /// Local camera capturer — held for the call's lifetime so it
    /// outlives `openPeerConnectionAndPublish` and is torn down in
    /// `hangUp`. `nil` for an audio-only call (or when no camera exists).
    private var localCameraCapture: LocalCameraCapture? {
        didSet {
            guard oldValue !== localCameraCapture else { return }
            oldValue?.onInterruptionChanged = nil
            localCameraCapture?.onInterruptionChanged = { [weak self] interrupted in
                MainActor.assumeIsolated { self?.setCameraInterrupted(interrupted) }
            }
            if localCameraCapture == nil { setCameraInterrupted(false) }
        }
    }

    /// iOS interrupted the camera (`docs/ux/calls.md` section 4.5.4): the app
    /// left the screen and may not keep the camera. While it lasts every
    /// other peer is told the camera is off, so they see the avatar rather
    /// than a frozen frame; ``localCameraOn``, and so the camera button here,
    /// is left alone, because the person did not turn the camera off.
    private var cameraInterrupted = false

    /// The camera was interrupted (`true`) or came back (`false`).
    func setCameraInterrupted(_ interrupted: Bool) {
        guard cameraInterrupted != interrupted else { return }
        cameraInterrupted = interrupted
        broadcastMediaState()
    }
    private(set) var state: State = .idle
    /// The session is connecting or connected: not yet started, hung up and
    /// failed are all "no call to capture or publish for".
    private var isLive: Bool { state == .connecting || state == .connected }
    /// ``openPeerConnectionAndPublish()`` is between its start and the peer
    /// connection existing (it awaits the relay credential first), so a
    /// second ``publish(seed:localLeaf:)`` in that window does not build a
    /// second connection.
    private var openingPeerConnection = false
    /// Whether the SFU refused this device because the call had already
    /// ended (`ErrorCode::CallEnded`). Read by the host after a failed
    /// `connect()` to tell that from a connection failure.
    private(set) var endedByServer = false

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

    // ---- Signaling keepalive (`SfuKeepalive`) ------------------------------
    /// Ticks every `SfuKeepalive.tickMs` from signaling connect to hang-up.
    private var keepaliveTask: Task<Void, Never>?
    /// When the server last proved it was there (Welcome or Pong).
    private var lastInboundMs: Int64 = 0
    /// The last time the SFU proved it was there (the Welcome or a Pong), on
    /// this session's clock; 0 before the socket opened. What the call record
    /// ends at when the link died before the device noticed
    /// (`SfuKeepalive.callEndedAtMs`).
    var lastInboundAtMs: Int64 { lastInboundMs }
    /// When this device last pinged.
    private var lastPingMs: Int64 = 0
    private var pingNonce: UInt64 = 0
    private let nowMs: () -> Int64

    init(
        config: Config,
        callbacks: Callbacks,
        nowMs: @escaping () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) },
        rules: TileQualityRules = SharedTileQualityRules()
    ) {
        self.rules = rules
        self.config = config
        self.callbacks = callbacks
        self.activeSeed = config.seed
        if let seed = config.seed { relay.setSeed(seed) }
        self.publishLeaf = config.localLeaf ?? 0
        self.localCameraOn = config.video
        self.localMicOn = config.audio
        self.nowMs = nowMs
    }

    /// Broadcast our current camera and microphone state to every other peer
    /// over the opaque SFU `signal-relay` (camera-off-cleanup; the microphone
    /// since 2026-11-07, `lakko_contacts::media_state` version 2). Best-effort — a no-op
    /// if signaling is not open. `sendSignalRelayToCall` base64s the
    /// `composeMediaState` bytes into the relay payload; the SFU forwards it
    /// verbatim without inspection (invariant #2). Called on every local
    /// camera transition, once when the call connects, and again whenever a
    /// new peer joins (so a newcomer is initial-synced deterministically).
    func broadcastMediaState() {
        guard signaling != nil || mediaStateSendSeam != nil else { return }
        // Sealed under the call's relay key (ADR-0078 section 2): the SFU
        // relays ciphertext only. No key yet (a link joiner before its
        // Welcome) means nothing is said.
        guard let sealed = relay.seal(
            composeMediaState(cameraOn: localCameraOn && !cameraInterrupted, microphoneOn: localMicOn))
        else { return }
        if let seam = mediaStateSendSeam {
            seam(sealed)
            return
        }
        sendSignalRelayToCall(payload: sealed)
    }

    /// Test seam: when set, a sealed media-state envelope goes here instead
    /// of onto the signaling socket (a unit test has no SFU). `nil` in the
    /// app.
    var mediaStateSendSeam: ((Data) -> Void)?

    /// Broadcast this device's sealed peer announcement (ADR-0078 section 2).
    /// Best-effort: without an announcer, a self peer id, a relay key or an
    /// open socket nothing is sent.
    private func announceSelf() {
        guard signaling != nil, let announcer = config.peerAnnouncer, let selfPeerId,
              let announcement = announcer.compose(selfPeerId),
              let sealed = relay.seal(announcement)
        else { return }
        sendSignalRelayToCall(payload: sealed)
        // The name for this call follows the announcement it is checked
        // against (a newcomer, a rekey, the periodic re-announcement).
        callNames.resend()
    }

    /// Set this device's name for the call (`docs/ux/calls.md` section 4.1):
    /// `name` has passed the display-name rule; `""` goes back to the
    /// default. Sent at once when the call can carry it, and again after
    /// every announcement, so someone who joins later sees it too.
    func setOwnCallName(_ name: String) {
        callNames.setOwnName(name)
    }

    /// Send a message to the call's chat (`docs/ux/calls.md` section 4.1.4):
    /// `text` has passed the call-chat rule. Returns its sequence number and
    /// whether it went out (no socket, no relay key: not sent).
    func sendCallChat(_ text: String) -> (seq: UInt64, sent: Bool) {
        callChats.send(text)
    }

    /// `Send again` on the chat message numbered `seq`: the same signed bytes.
    func resendCallChat(seq: UInt64) -> Bool {
        callChats.resend(seq: seq)
    }

    private func startAnnouncing() {
        announceTask?.cancel()
        announceTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(Self.peerAnnounceIntervalMs) * 1_000_000)
                if Task.isCancelled { return }
                self?.announceSelf()
            }
        }
    }

    // MARK: - What this device shows, and what the SFU gives

    /// A tile says which picture of `peerId` it shows (``VideoWants``), or
    /// `nil` when it stops showing one. `tile` is the identity of any object
    /// that lives as long as the tile. The session asks the SFU for the
    /// highest quality any tile of a person wants, `light` for a person no
    /// tile shows, and tells it whenever that changes.
    func reportTileVideoQuality(
        tile: ObjectIdentifier, peerId: String, quality: TileVideoQuality?
    ) {
        if wantsBook.setTile(tile, peerId: peerId, quality: quality) { wantsSender.changed() }
    }

    /// The picture of `peerId` this device asks the SFU for now.
    func videoQuality(of peerId: String) -> TileVideoQuality {
        wantsBook.quality(of: peerId)
    }

    /// What the SFU gives of `peerId` now; `nil` when it has not said
    /// (downlink control off, or a person its last frame did not name).
    func videoGiven(of peerId: String) -> TileVideoQuality? {
        givenBook.given(of: peerId)
    }

    /// A `video-given` frame: the SFU's whole set replaces the last one. The
    /// tiles are told, and one line says whose shown picture it changed. What
    /// this device asks for is left as it is.
    private func onVideoGivenFrame(_ entries: [SfuVideoGiven]) {
        let before = givenBook
        givenBook.replace(entries)
        if let line = VideoGiven.describe(
            "call video given",
            VideoGiven.changes(
                wants: wantsBook.wants(),
                before: { before.given(of: $0) },
                after: { self.givenBook.given(of: $0) })) {
            NSLog("%@", line)
        }
        callbacks.onVideoGiven?(givenBook.snapshot)
    }

    /// Put one `video-wants` frame on the wire; `false` when it cannot go
    /// yet (the sender then tries again at its next call).
    private func sendVideoWants(_ frame: [SfuVideoWant]) -> Bool {
        guard let signaling, publishOfferSent else { return false }
        guard signaling.sendVideoWants(frame) else { return false }
        NSLog("%@", VideoWants.describe(frame))
        return true
    }

    // MARK: - Which camera senders send

    /// Whether the middle sender sends now: the shared rule
    /// (`middle_sender_wanted`) over the people in the call, this device's
    /// own person included.
    private func middleWanted() -> Bool {
        rules.middleSenderWanted(people: UInt32(clamping: wantsBook.people))
    }

    /// The "camera is on" condition of the sender-activity decision: this
    /// device has a camera track.
    ///
    /// Deliberately NOT the track's `isEnabled`. The person turning the
    /// camera off is `isEnabled = false` on the camera's tracks, exactly as
    /// before this rule existed: the senders go on sending (black frames, at
    /// very little bitrate) and a viewer who joins meanwhile still gets its
    /// key frame. The SFU's own bookkeeping for a silent sender covers only
    /// a sender it told `false`; a sender this client silenced by itself
    /// would be an unannounced stop on the SFU's side. So the person's
    /// camera toggle and the SFU's word compose without touching each other.
    private func cameraIsOn() -> Bool {
        pc?.currentLocalVideoTrack() != nil
    }

    private func activityInput() -> SenderActivityRule.Input {
        SenderActivityRule.Input(
            cameraOn: cameraIsOn(), middleWanted: middleWanted(), needed: videoNeeded)
    }

    /// Which camera senders encode and send now (``SenderActivityRule``).
    private func sendersActiveNow() -> [CameraSenderKind: Bool] {
        let connection = pc
        return SenderActivityRule.allActive(activityInput()) { kind in
            connection?.cameraSender(kind) != nil
        }
    }

    /// Make every camera sender's encoding active or not, as
    /// ``sendersActiveNow()`` says now. A parameter write only: the media
    /// lines, the SSRCs and the SFrame encryptors stay and nothing is
    /// renegotiated.
    ///
    /// Called after everything that can change the answer or reset what the
    /// binding holds: a `video-needed` frame, a change of the roster, a
    /// sender added, a renegotiation, and every media-log tick (which is
    /// also when a refused switch-on is asked again). A no-op for a sender
    /// whose encoding already says so.
    private func syncSenderActivity() {
        let active = sendersActiveNow()
        for kind in CameraSenderKind.allCases {
            guard let activity = senderActivity[kind] else { continue }
            activity.bind(pc?.cameraActivitySender(kind))
            if activity.bound { activity.want(active[kind] ?? false) }
        }
        // Every look at the senders is also when a refused ceiling is asked
        // again, and a sender added or negotiated again is exactly the moment
        // its bitrate ceiling may have been lost.
        syncSenderCeilings()
    }

    /// Put the main and the middle camera sender's encoding under the
    /// ceiling the SFU last gave (``PublishBudgetRule``). A parameter write
    /// only. A no-op before the first `publish-budget` frame and for a
    /// sender whose encoding already holds the ceiling; a write that did not
    /// take is asked again here, at every media-log tick at the latest. A
    /// sender created again gets the ceiling in force, not the one it was
    /// created with.
    private func syncSenderCeilings() {
        for kind in PublishBudgetRule.budgetedKinds {
            guard let ceiling = senderCeilings[kind] else { continue }
            ceiling.bind(pc?.cameraCeilingSender(kind))
            if ceiling.bound {
                ceiling.want(PublishBudgetRule.senderCeiling(kind, publishBudget))
            }
        }
    }

    /// A `publish-budget` frame: the SFU's whole word replaces the last one,
    /// the senders follow at once, and one line says so when it changed.
    private func onPublishBudgetFrame(_ budget: PublishBudget) {
        if let line = PublishBudgetRule.describeChange(before: publishBudget, budget) {
            NSLog("%@", line)
        }
        publishBudget = budget
        syncSenderCeilings()
    }

    /// A `video-needed` frame: the SFU's whole word replaces the last one,
    /// the senders follow at once, and one line says so when it changed
    /// which of them is active.
    private func onVideoNeededFrame(_ needed: VideoNeeded) {
        let before = sendersActiveNow()
        videoNeeded = needed
        if let line = SenderActivityRule.describeChange(
            needed, before: before, after: sendersActiveNow()) {
            NSLog("%@", line)
        }
        syncSenderActivity()
    }

    /// The people of the call changed: the middle sender starts or stops
    /// with the count, and the SFU is told what this device shows of the
    /// people now in it (again, whatever it is, after a join).
    private func onRosterChanged(joined: Bool) {
        syncSenderActivity()
        if joined { wantsSender.resend() } else { wantsSender.changed() }
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
        // The relay key follows the seed; announce under the new one.
        relay.setSeed(newSeed)
        announceSelf()
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
        startKeepalive()
        startAnnouncing()

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
        // A publish asked of a session that is over (the person left, the
        // admission timed out or the socket closed between the Welcome and
        // this call) captures nothing and reports nothing: the end was
        // already reported through the state.
        guard isLive else { return }
        guard pc == nil, !openingPeerConnection else { return }
        if let seed {
            activeSeed = seed
            relay.setSeed(seed)
            announceSelf()
        }
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

    /// Mute / unmute the local microphone (local-only track gate). The
    /// choice is recorded even before capture (a mute chosen in the prejoin
    /// or before the Welcome holds for the captured microphone), and a
    /// change is announced so the other tiles show the mute mark.
    func setMicrophoneEnabled(_ on: Bool) {
        let changed = localMicOn != on
        localMicOn = on
        pc?.setMicrophoneEnabled(on)
        if changed { broadcastMediaState() }
    }

    /// The microphone choice this session announces and applies (test seam).
    var microphoneEnabled: Bool { localMicOn }

    // MARK: - Signaling keepalive

    /// Start the keepalive clock the moment the socket is open; the first ping
    /// goes out one interval later, and a Welcome or Pong resets the deadline.
    private func startKeepalive() {
        let now = nowMs()
        lastInboundMs = now
        lastPingMs = now
        keepaliveTask?.cancel()
        keepaliveTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(SfuKeepalive.tickMs) * 1_000_000)
                if Task.isCancelled { return }
                self?.onKeepaliveTick()
            }
        }
    }

    private func onKeepaliveTick() {
        if state == .failed || state == .closed { return }
        switch SfuKeepalive.action(nowMs: nowMs(), lastInboundMs: lastInboundMs, lastPingMs: lastPingMs) {
        case .ping:
            pingNonce &+= 1
            signaling?.sendPing(nonce: pingNonce)
            lastPingMs = nowMs()
        case .dead:
            NSLog(
                "SfuCallSession: keepalive dead (no pong for %d s); failing the call",
                Int((nowMs() &- lastInboundMs) / 1000))
            fail()
        case .wait:
            break
        }
    }

    private func noteSignalingInbound() {
        lastInboundMs = nowMs()
    }

    /// Tear down. Idempotent.
    func hangUp() {
        keepaliveTask?.cancel()
        keepaliveTask = nil
        announceTask?.cancel()
        announceTask = nil
        relay.dispose()
        // The call's chat goes with the call.
        callChats.clear()
        stopMediaLog()
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
        // Only a session that set the default output puts it back. One that
        // never opened a peer connection (an attempt abandoned during its
        // connect, hung up after a newer call started) would otherwise take
        // the loudspeaker default away from the call that is up now.
        let setCallOutput = pc != nil
        pc?.close()
        pc = nil
        if setCallOutput {
            // The next call is not necessarily a group one: hand the default
            // output back to the earpiece.
            CallAudioSession.configureCallOutput(loudspeakerByDefault: false)
        }
        audioTrackPeers.removeAll()
        lightTrackIds.removeAll()
        middleTrackIds.removeAll()
        peerSignerKeys.removeAll()
        avAlign.clear()
        avReceivers.removeAll()
        offerQueue.removeAll()
        videoNeeded = nil
        for activity in senderActivity.values { activity.reset() }
        publishBudget = nil
        for ceiling in senderCeilings.values { ceiling.reset() }
        publishOfferSent = false
        wantsSender.reset()
        wantsBook.clear()
        if givenBook.count > 0 {
            givenBook.clear()
            callbacks.onVideoGiven?([:])
        }
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

    /// TURN-credential client, injected by the call-UI layer. `nil` means
    /// host candidates only.
    ///
    /// The SFU has a public address and learns a client's outside address from
    /// its own packets, so most clients reach it without any server-reflexive
    /// candidate - but for one whose network blocks that path the relay is the
    /// only way into the room.
    private var turnClient: TurnCredentialClient?

    public func setTurnClient(_ client: TurnCredentialClient?) {
        self.turnClient = client
    }

    /// Lakko's own servers only (``IceServers``): the minted TURN row and
    /// STUN on its host, or none. Fail-soft: a client that answers `nil`
    /// leaves the call on host candidates rather than refusing it.
    private func resolveIceServers() async -> [RTCIceServer] {
        await IceServers.resolve(turnClient)
    }

    private func openPeerConnectionAndPublish() async throws {
        guard let seed = activeSeed else {
            // Never publish media without a seed (invariant #4).
            throw SfuCallError.sframeUnavailable
        }
        openingPeerConnection = true
        defer { openingPeerConnection = false }
        let iceServers = await resolveIceServers()
        // The call may be over by now: the person pressed End, the socket
        // closed, or the SFU answered `call-ended` in place of the Welcome
        // while the relay credential was fetched. `hangUp()` ran with no
        // peer connection to close, so building one now would leave the
        // microphone and the camera running with no call and nothing left to
        // stop them.
        guard isLive else { throw SfuCallError.endedWhileConnecting }
        // A group call plays on the loudspeaker unless a headset or a
        // Bluetooth device is connected, which then takes over by itself; it
        // has no Speaker control to move the sound with. Set before the peer
        // connection exists, so it is what WebRTC applies when it brings the
        // audio up.
        CallAudioSession.configureCallOutput(loudspeakerByDefault: true)
        let connection = WebRTCPeerConnection(iceServers: iceServers)
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
        // A mute chosen before the capture (the person's choice reaches
        // `setMicrophoneEnabled` as soon as the call exists) holds for the
        // captured microphone: what is announced is what is sent.
        connection.addLocalAudioTrack(enabled: localMicOn)
        if config.video {
            startLocalCameraCapture(on: connection)
        }
        guard connection.armSFrameEncryptors(seed: seed, localLeaf: publishLeaf) else {
            // A failed arm is fail-closed (invariant #4).
            throw SfuCallError.sframeUnavailable
        }
        let offer = try await connection.createOffer(audio: config.audio, video: config.video)
        // Hung up or failed while the offer was built: `hangUp()` closed
        // this connection and reset the negotiation state, which must stay
        // reset.
        guard pc === connection else { throw SfuCallError.endedWhileConnecting }
        // Mark our initial publish offer in flight so an SFU subscribe-offer
        // that arrives before the answer waits in `offerQueue`
        // (`drainServerOffers` guards on `makingOffer`). The first
        // `applyRemoteAnswer` clears it + arms `negotiationReady` for a
        // mid-call renegotiation (piece B: `publishVideo`).
        makingOffer = true
        // The copies' transceivers have their `mid`s now.
        signaling?.sendSdpOffer(
            offer, lightMids: connection.cameraCopyMids(.light),
            middleMids: connection.cameraCopyMids(.middle))
        // The SFU has media state for this peer from this offer on: tell it
        // what this device shows of everyone.
        publishOfferSent = true
        wantsSender.resend()
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
        // Three ordinary senders (ADR-0070 steps 1 and 5); the SFrame arm
        // that follows in the caller seals each under its own stream index.
        addCameraSenders(on: connection, source: source, capture: capture)
        localCameraCapture = capture
        callbacks.onLocalVideoTrack?(connection.currentLocalVideoTrack())
        Task { [weak self] in
            do {
                try await capture.start()
            } catch {
                NSLog("SfuCallSession: camera start failed: %@", String(describing: error))
                await MainActor.run {
                    // Only while this capture is still the session's: after a
                    // hang-up the preview was already cleared.
                    guard let self, self.localCameraCapture === capture else { return }
                    self.localCameraCapture = nil
                    self.callbacks.onLocalVideoTrack?(nil)
                }
            }
        }
    }

    /// Add the camera's main sender and its two copies, in that order. The
    /// middle one is created sending or not as the roster and the SFU say
    /// now; the main and the light one are created sending.
    ///
    /// The main sender is on `source`; each copy is on a source of its own
    /// that `capture` feeds the same frames (``CapturerFan``), so one
    /// encoder's adaptation narrows only its own picture.
    private func addCameraSenders(
        on connection: WebRTCPeerConnection, source: RTCVideoSource,
        capture: LocalCameraCapture
    ) {
        connection.addLocalCameraTrack(source)
        var input = activityInput()
        // The camera is being turned on by this very call.
        input.cameraOn = true
        connection.addCameraCopies(
            sources: { capture.copySource($0) },
            middleActive: SenderActivityRule.active(.middle, exists: true, input))
        NSLog(
            "SfuCallSession: camera senders added main=%@ light=%@ middle=%@",
            connection.cameraSender(.main) != nil ? "yes" : "no",
            connection.cameraSender(.light) != nil ? "yes" : "no",
            connection.cameraSender(.middle) != nil ? "yes" : "no")
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
            // Hung up while the camera was starting: `hangUp()` already put
            // the state back, and a camera that did not start for a call that
            // is over is not a failure to show.
            guard pc === connection else { return }
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
        // already-armed audio encryptor). On an arm failure REMOVE the
        // just-added sender, tear the camera down and do NOT renegotiate.
        // Skipping only this renegotiation is not enough: the sender would
        // stay on the peer connection, and the next offer for any other
        // reason (or the answer to the SFU's next subscribe offer) would
        // negotiate an unsealed video m-line. The removal is synchronous and
        // comes before the first suspension point, so nothing can negotiate
        // in between (fail-closed).
        //
        // The camera goes out as three senders, as on the first publish
        // (ADR-0070). The arm below seals the main one, which decides the
        // outcome, and each copy; a copy that cannot be sealed is removed
        // there and the camera is published without it.
        addCameraSenders(on: liveConn, source: source, capture: capture)
        localCameraCapture = capture
        guard liveConn.armSFrameVideoEncryptor(seed: seed, localLeaf: publishLeaf) else {
            liveConn.removeLocalVideoTrack()
            localCameraCapture = nil
            callbacks.onLocalVideoTrack?(nil)
            await capture.stop()
            // The call may have ended while the camera stopped; a hang-up
            // already put the state back to `.off`.
            if pc === liveConn { setVideoPublishState(.failed) }
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
        // A fresh encryptor starts without its codec (ADR-0071). A sender
        // that reuses a negotiated media line already has one; the others
        // get theirs when the renegotiation below is answered.
        liveConn.syncSenderCodecs()
        // The new senders get their flags as the roster and the SFU say now.
        syncSenderActivity()
        // Renegotiate so the SFU (and thus other peers) learn the new video
        // m-lines. Serialized behind any in-flight offer.
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
            // Hung up while the offer was built: nothing to send, and the
            // flags `hangUp()` reset stay reset.
            guard pc === connection else { return }
            signaling.sendSdpOffer(
                offer, lightMids: connection.cameraCopyMids(.light),
                middleMids: connection.cameraCopyMids(.middle))
            // `makingOffer` stays true until `applyRemoteAnswer` clears it.
        } catch {
            guard pc === connection else { return }
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
            // A `publish-budget` frame without both ceilings, or with one
            // out of the protocol's bounds, changes nothing: the senders
            // keep the ceilings the last good frame gave them.
            if PublishBudgetRule.claimsPublishBudget(raw) {
                NSLog("%@", PublishBudgetRule.malformedWarning)
            }
        }
        cb.onPong = { [weak self] _ in
            Task { @MainActor in self?.noteSignalingInbound() }
        }
        cb.onWelcome = { [weak self] selfPeerId, _, roster in
            Task { @MainActor in self?.noteSignalingInbound() }
            // The joiner learns about everyone already in the call HERE, not
            // from peer-joined — a second joiner gets no peer-joined for the
            // peers that preceded it. `rosterCount` is therefore the whole
            // answer to "did this device learn who else is on the call". No
            // identifier in the line: not the room, not a peer id.
            NSLog("SfuCallSession: welcome rosterCount=%d", roster.count)
            Task { @MainActor in
                guard let self else { return }
                self.selfPeerId = selfPeerId
                self.wantsBook.setPeers(roster.map(\.peerId).filter { $0 != selfPeerId })
                self.onRosterChanged(joined: false)
                self.callbacks.onWelcome?(selfPeerId, roster)
                // Say who this device is (ADR-0078 section 2).
                self.announceSelf()
            }
        }
        cb.onVideoGiven = { [weak self] given in
            Task { @MainActor in self?.onVideoGivenFrame(given) }
        }
        cb.onVideoNeeded = { [weak self] main, middle in
            Task { @MainActor in
                self?.onVideoNeededFrame(VideoNeeded(main: main, middle: middle))
            }
        }
        cb.onPublishBudget = { [weak self] mainKbps, middleKbps in
            guard let budget = PublishBudgetRule.read(mainKbps: mainKbps, middleKbps: middleKbps)
            else {
                NSLog("%@", PublishBudgetRule.malformedWarning)
                return
            }
            Task { @MainActor in self?.onPublishBudgetFrame(budget) }
        }
        cb.onPeerJoined = { [weak self] peerId in
            NSLog("SfuCallSession: peer joined")
            Task { @MainActor in
                guard let self else { return }
                if peerId != self.selfPeerId {
                    self.wantsBook.addPeer(peerId)
                    self.onRosterChanged(joined: true)
                }
                self.callbacks.onPeerJoined?(peerId)
                // Camera-off-cleanup: re-announce our current camera state so
                // the newcomer (which was not in our Welcome-roster broadcast)
                // is initial-synced deterministically; and who we are.
                self.broadcastMediaState()
                self.announceSelf()
            }
        }
        cb.onPeerLeft = { [weak self] peerId in
            NSLog("SfuCallSession: peer left")
            Task { @MainActor in
                guard let self else { return }
                self.wantsBook.removePeer(peerId)
                // Their sound-and-picture rule goes with them, and their
                // receivers with it.
                self.avAlign.removePeer(peerId)
                self.avReceivers = self.avReceivers.filter { $0.value.peerId != peerId }
                // What the SFU gave of them goes with them: someone who
                // later joins under the same id starts with nothing said.
                if self.givenBook.removePeer(peerId) {
                    self.callbacks.onVideoGiven?(self.givenBook.snapshot)
                }
                self.onRosterChanged(joined: false)
                // A participant that leaves ends their share (ADR-0083
                // section 2).
                if let signer = self.peerSignerKeys.removeValue(forKey: peerId),
                    self.screenShares.participantLeft(signerKey: signer) {
                    self.callbacks.onScreenShareChanged?(self.screenShares.current)
                }
                self.callNames.peerLeft(peerId)
                self.callChats.peerLeft(peerId)
                self.callbacks.onPeerLeft?(peerId)
            }
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
            Task { @MainActor in
                guard let self else { return }
                if code == .callEnded {
                    // The SFU's answer in place of the Welcome: the room stood
                    // empty past its grace before this device arrived. Not a
                    // failure of this device's; the host closes the chat's
                    // record and says the call ended.
                    self.endedByServer = true
                    self.fail()
                    return
                }
                self.callbacks.onServerError?(code, detail)
            }
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
    func dispatchSignalRelay(from: String, payloadB64: String) {
        guard let payload = try? sfuDecodePayload(payloadB64: payloadB64) else { return }
        // ADR-0078 section 2: a payload sealed under the call's relay key is
        // consumed here (the camera media state, a peer announcement); an
        // unsealed media state is not accepted. Everything else passes through
        // to `onSignalRelay` unchanged.
        if let opened = relay.open(payload) {
            switch opened {
            case .mediaState(let cameraOn, let microphoneOn):
                callbacks.onRemoteMediaState?(from, cameraOn)
                callbacks.onRemoteMicrophoneState?(from, microphoneOn)
            case .announcement(let announcement):
                if let identity = config.peerAnnouncer?.verify(from, announcement) {
                    if !identity.signatureKey.isEmpty { peerSignerKeys[from] = identity.signatureKey }
                    callbacks.onPeerIdentified?(from, identity)
                    callNames.peerIdentified(from, signerKey: identity.signatureKey)
                    callChats.peerIdentified(from, signerKey: identity.signatureKey)
                }
            case .screenShare(let state):
                receiveScreenShare(from: from, state: state)
            case .callName(let name):
                callNames.received(from: from, bytes: name)
            case .callChat(let message):
                callChats.received(from: from, bytes: message)
            case .unknown:
                break
            }
            return
        }
        callbacks.onSignalRelay?(from, payload)
    }

    /// A sealed screen-share state from `from` (ADR-0083 section 1). It is
    /// accepted only once `from` has a verified announcement, and only when it
    /// verifies against the call's group with that announcement's leaf key and
    /// `from` as its peer id; anything else is dropped.
    private func receiveScreenShare(from: String, state: Data) {
        guard let signer = peerSignerKeys[from],
            let verified = config.peerAnnouncer?.verifyScreenShare(from, signer, state)
        else {
            NSLog("SfuCallSession: screen-share state dropped (unverified)")
            return
        }
        if screenShares.observe(verified) {
            NSLog(
                "SfuCallSession: current screen share changed: live=%@",
                screenShares.current != nil ? "true" : "false")
            callbacks.onScreenShareChanged?(screenShares.current)
        }
    }

    private func applyRemoteAnswer(_ sdp: String) async {
        guard let pc = pc else { return }
        do {
            try await pc.applyRemoteAnswer(sdp)
        } catch {
            // An apply that failed because the call was hung up meanwhile is
            // not a failure of the call.
            if self.pc === pc { fail() }
            return
        }
        // Hung up while the answer was applied: the negotiation state
        // `hangUp()` reset stays reset.
        guard self.pc === pc else { return }
        // The initial publish (or a mid-call renegotiation) is now answered:
        // clear the in-flight flag + arm renegotiation for future track
        // changes (piece B). Then either drive a camera-on that queued while we
        // were mid-offer, or drain any SFU subscribe-offers that waited.
        // The senders' codecs are negotiated now: their SFrame encryptors
        // need them to leave the right codec header in the clear (ADR-0071).
        pc.syncSenderCodecs()
        // A flag this negotiation reset on a sender is put back.
        syncSenderActivity()
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
                // Hung up while the answer was built.
                guard self.pc === pc else { break }
                signaling.sendSdpAnswer(answer)
                NSLog("SfuCallSession: answered server offer bytes=%d", answer.count)
                // Harmless when nothing changed for the senders (ADR-0071).
                pc.syncSenderCodecs()
                syncSenderActivity()
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

    // MARK: - Sound levels

    /// The sound level of this device's microphone and of each other person's
    /// audio, as the WebRTC engine measured them here: the remote ones on
    /// audio already received, decrypted and decoded
    /// (`docs/ux/group-call-grid.md` section 7.3). `nil` when there is no
    /// peer connection. Nothing about it leaves the device, and the call
    /// server is never asked who is speaking.
    func sampleAudioLevels() async -> CallAudioLevels? {
        guard let pc, let entries = await pc.sampleAudioStatEntries() else { return nil }
        return CallAudioLevelStats.levels(from: entries, peerIdByTrackId: audioTrackPeers)
    }

    // MARK: - Media log

    /// Log what the video receivers, the camera sender and the tiles report,
    /// once per interval and as a total when the call ends (ADR-0067 steps 1
    /// and 5; the lines of ``CallMediaCounters``). Diagnostics only.
    private func startMediaLog() {
        guard mediaLogTask == nil else { return }
        // The tiles of this call start from nothing.
        LayerDisplayLedger.shared.reset()
        mediaLogTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: CallMediaCounters.intervalMs * 1_000_000)
                if Task.isCancelled { return }
                await self?.sampleMediaLog()
            }
        }
    }

    private func sampleMediaLog() async {
        guard let connection = pc, let entries = await connection.sampleMediaStatEntries() else {
            return
        }
        // What each sender's encoding holds, read behind every write queued
        // so far.
        let ceilings = await connection.cameraSenderCeilings()
        let pathEntries = await connection.samplePathStatEntries()
        // The call may have ended while the report was read.
        guard pc === connection, mediaLogTask != nil else { return }
        if let pathEntries, let line = callPathLog.note(pathEntries) {
            NSLog("%@", line)
        }
        // A sender the binding refused to start is asked again, and a flag a
        // renegotiation reset is put back.
        syncSenderActivity()
        let reading = CallMediaCounters.read(
            entries, lightTrackIds: lightTrackIds, middleTrackIds: middleTrackIds,
            senderOfMid: connection.cameraSenderMids())
        let nowMs = RemoteLayerController.monotonicMs()
        let elapsedMs = mediaLogLastAtMs.map { nowMs >= $0 ? nowMs - $0 : 0 } ?? 0
        let label = CallMediaCounters.intervalLabel
        let active = sendersActiveNow()
        let needed = videoNeeded
        var lines = CallMediaCounters.describeInbound(label, before: mediaLogLast, now: reading)
        lines += CallMediaCounters.describeOutbound(
            label, before: mediaLogLast, now: reading, elapsedMs: elapsedMs,
            activityOf: { kind in
                OutboundActivity(
                    needed: SenderActivityRule.neededWord(kind, needed),
                    active: active[kind] ?? false)
            },
            existing: CameraSenderKind.allCases.filter { connection.cameraSender($0) != nil },
            ceilingOf: { kind in ceilings[kind] ?? nil })
        // What each remote tile showed over the interval, with what this
        // device asks for of that person and what the SFU gives.
        lines += CallMediaCounters.describeShownPeers(
            label, LayerDisplayLedger.shared.takeInterval(),
            wantOf: { self.wantsBook.quality(of: $0) },
            givenOf: { self.givenBook.given(of: $0) })
        // Each person's sound against their pictures, as the last reading of
        // the shared rule had it.
        lines += avAlign.lines(label)
        for line in lines { NSLog("%@", line) }
        mediaLogLast = reading
        mediaLogLastAtMs = nowMs
    }

    // MARK: - Sound and picture together

    /// Read the receivers once per reading interval of the shared rule and
    /// hand each remote person's readings to their `AvAlign` (ADR-0070, note
    /// of 2026-10-12). What the rule can do here is said once, at the start.
    private func startAvAlign() {
        guard avAlignTask == nil else { return }
        let intervalMs = AvAlignStats.readingIntervalMs()
        NSLog(
            "%@",
            AvAlignLog.describeCapability(
                "call media", audioSettable: AvReceiverDelay.audioSettable,
                videoSettable: AvReceiverDelay.videoSettable, intervalMs: intervalMs))
        avAlignTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: intervalMs * 1_000_000)
                if Task.isCancelled { return }
                await self?.sampleAvAlign()
            }
        }
    }

    private func stopAvAlign() {
        avAlignTask?.cancel()
        avAlignTask = nil
    }

    private func sampleAvAlign() async {
        // One read of the whole connection's report for every receiver.
        guard let connection = pc, let entries = await connection.sampleInboundStatEntries()
        else { return }
        // The call may have ended while the report was read.
        guard pc === connection, avAlignTask != nil else { return }
        let writes = avAlign.read(
            nowMs: RemoteLayerController.monotonicMs(),
            samples: AvAlignStats.inboundSamples(entries),
            shownOf: { peerId in
                AvAlignStats.shownSlot(
                    AvShownFacts(
                        hasTile: self.wantsBook.hasTile(of: peerId),
                        wanted: self.wantsBook.quality(of: peerId),
                        given: self.givenBook.given(of: peerId),
                        hasMiddle: self.avAlign.has(peerId, .middle),
                        hasLight: self.avAlign.has(peerId, .light),
                        lightOnTop: LayerDisplayLedger.shared.lightShown(peerId: peerId)))
            })
        // Carry the answers out. A set blocks until the engine's worker
        // thread has run it, so it runs off the main thread, one after the
        // other in the order the rule gave them. One that does not take
        // (no receiver for the track, the setter not available in this
        // process, or the engine's object failing its probe) is said once
        // per person and kind, and that person's rule goes on as one that
        // cannot set it.
        for write in writes {
            // The call may have ended, or the person left, during a set.
            guard pc === connection, avAlignTask != nil else { return }
            var why = "no-receiver"
            if let held = avReceivers[write.trackId], held.peerId == write.peerId {
                let status = await connection.setReceiverPlayoutDelay(
                    held.receiver, ms: write.setMs)
                if AvReceiverDelay.took(status) { continue }
                why = AvReceiverDelay.name(status)
            }
            if let capability = avAlign.writeFailed(peerId: write.peerId, slot: write.slot) {
                NSLog(
                    "%@",
                    AvAlignLog.describeWriteFailed(
                        "call media", peerId: write.peerId, slot: write.slot,
                        capability: capability, why: why))
            }
        }
    }

    /// Stop the reads and log the whole call's totals from what was read.
    private func stopMediaLog() {
        stopAvAlign()
        guard let task = mediaLogTask else { return }
        task.cancel()
        mediaLogTask = nil
        let label = CallMediaCounters.totalLabel
        var lines: [String] = []
        if let last = mediaLogLast {
            // Counted from zero: the counters are cumulative from each
            // stream's start.
            lines += CallMediaCounters.describeInbound(label, before: nil, now: last)
        }
        lines += CallMediaCounters.describeShownPeers(
            label, LayerDisplayLedger.shared.totals(),
            wantOf: { self.wantsBook.quality(of: $0) },
            givenOf: { self.givenBook.given(of: $0) })
        for line in lines { NSLog("%@", line) }
        mediaLogLast = nil
        mediaLogLastAtMs = nil
        callPathLog = CallPathLog()
        LayerDisplayLedger.shared.reset()
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
                guard let self else { return }
                // A stream id ending in `~light` is the light companion of
                // the peer named before the suffix, not a stream of its own.
                // One ending in `~mid` is that peer's middle track.
                switch RemoteVideoRoute.of(streamId: streamId) {
                case .main(let mainStreamId):
                    self.callbacks.onRemoteVideoTrack?(track, mainStreamId)
                case .light(let peerId):
                    guard let track else { return }
                    NSLog("SfuCallSession: light companion track attached peer=%@", peerId)
                    self.callbacks.onRemoteLightVideoTrack?(track, peerId)
                case .middle(let peerId):
                    guard let track else { return }
                    NSLog("SfuCallSession: middle track attached peer=%@", peerId)
                    self.callbacks.onRemoteMiddleVideoTrack?(track, peerId)
                case .screen(let peerId):
                    NSLog(
                        "SfuCallSession: screen track %@ peer=%@",
                        track != nil ? "attached" : "removed", peerId)
                    self.callbacks.onRemoteScreenVideoTrack?(track, peerId)
                }
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
                // Remember whose voice this track is, so a sound level read
                // from the statistics can be put on the right tile.
                let role = streamId.map { LightStream.split($0).role } ?? .main
                if kind == "audio", role == .main, let trackId = receiver.track?.trackId,
                    let streamId {
                    self.audioTrackPeers[trackId] = streamId
                }
                // A light companion's and a middle track's decryptor is armed
                // exactly like any receiver's, below. Its track id is
                // remembered so the media log can name the layer.
                if let trackId = receiver.track?.trackId {
                    switch role {
                    case .light: self.lightTrackIds.insert(trackId)
                    case .middle: self.middleTrackIds.insert(trackId)
                    // A screen's picture and sound (stream indices 2 and 5)
                    // are decrypted by the same decryptor as any receiver,
                    // below; its sound plays with the call.
                    case .main, .screen: break
                    }
                    // Which of the person's receivers this is, for the rule
                    // that keeps their sound and picture together. A track
                    // that replaces one under the same slot is told to the
                    // rule there.
                    if let streamId, let slot = AvSlot.of(kind: kind, role: role) {
                        let peerId = LightStream.split(streamId).peerId
                        self.avAlign.setReceiver(
                            peerId: peerId, slot: slot, trackId: trackId)
                        self.avReceivers[trackId] = (peerId, receiver)
                    }
                }
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
                // A companion or a middle track that cannot be decrypted is
                // never shown (it presents no frame); the peer is not marked
                // for it, since their main stream may be fine and the stream
                // id names no participant.
                if role != .main { return }
                self.callbacks.onUndecryptableReceiver?(streamId, kind)
            }
        )
    }

    private func onConnectionState(_ rtcState: RTCPeerConnectionState) {
        switch rtcState {
        case .connected:
            if state == .connecting { setState(.connected) }
            startMediaLog()
            startAvAlign()
            // Say again what this device shows, now that media flows.
            wantsSender.resend()
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
