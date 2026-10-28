// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import java.security.SecureRandom
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import org.webrtc.PeerConnection
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallMediaFlags
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody
import uniffi.lakko_ffi_uniffi.VerifiedCallScreenShare
import uniffi.lakko_ffi_uniffi.callSafetyNumber
import uniffi.lakko_ffi_uniffi.composeCallSignaling
import uniffi.lakko_ffi_uniffi.parseCallSignaling

/**
 * 1×1 P2P call session on Android.
 *
 * Mirror of:
 *   * `apps/ios/Lakko/Calls/CallSession.swift`
 *   * `apps/web/src/calls/call-session.ts`
 *
 * ## G3-2 status
 *
 * Real WebRTC integration: [dial] / [acceptCall] now build
 * real SDP via [WebRtcPeerConnection], local ICE candidates
 * are emitted through the `IceCandidate` signaling body, and
 * the underlying PeerConnection's connection-state
 * transitions drive [WebCallState]. Video is wired:
 * [dial]`(video=true)` / [acceptCall]`(withVideo=true)` run
 * [startLocalCameraCapture] (front camera 720p/30 via
 * `CameraVideoCapture`), which calls
 * [WebRtcPeerConnection.addLocalVideoTrack] before the SDP
 * build so the offer/answer advertises the m=video line;
 * audio-only calls simply skip it.
 *
 * ## SFrame end-to-end encryption
 *
 * SFrame keying is wired and mandatory (G3-sframe-native
 * arc N-1..N-6). The caller arms the per-call MLS-exporter
 * seed via [setCallKey] before [dial] / [acceptCall]; both
 * paths then run [attachSFrame], which attaches the SFrame
 * encryptors to every local sender via
 * [WebRtcPeerConnection.armSFrameEncryptors] and **refuses
 * the call** (no plaintext-media fallback) if the seed is
 * unset or the boot-time `SFrameAvailability` probe reports
 * the libwebrtc frame-encryptor selectors are missing —
 * security invariant #4.
 *
 * ## Threading
 *
 * Public methods are not annotated `@MainThread` but the
 * intended caller is the Compose UI thread. State flows
 * are read-only `StateFlow`s; mutations are funneled
 * through the constructor-supplied `onOutboundSignaling`
 * callback so the caller controls when bytes leave the
 * session. libwebrtc observer callbacks fire on internal
 * worker threads — the wrapper bounces them back into a
 * session-owned [CoroutineScope] running on `Dispatchers.Main`
 * before mutating any [StateFlow].
 */
public class CallSession(
    private val applicationContext: Context,
    override val callId: ByteArray,
    /**
     * Each outbound frame, with whether it rings someone or stops a ring
     * ([OneToOneCallWakes]): only the dial offer and the End of a call that
     * never connected here ask for a call wake.
     */
    private val onOutboundSignaling: (ByteArray, CallSignalingWake) -> Unit,
    /**
     * This device's own leaf signature key in the conversation, the screen
     * share fold's own key (ADR-0083). `null`: no share is folded, and the
     * other person's share is never shown.
     */
    private val ownSignatureKey: ByteArray? = null,
) : OneToOneCallSession {

    /** Lifecycle phases. Mirror of `lakko_calls::CallState`. */
    public enum class WebCallState { Idle, Outgoing, Ringing, Connecting, Connected, Ending, Done }

    /** Reason carried in the terminal transition. */
    public enum class WebCallEndReason { HungUp, Busy, Declined, TimedOut, Error }

    /**
     * ADR-0032 piece B2a — mid-call camera-on lifecycle. The Kotlin twin of
     * the iOS `CallSession.VideoPublishState` and the SFU session's
     * [SfuCallSession.VideoPublishState]. A 1×1 call started audio-only has no
     * video sender; turning the camera on ACQUIRES the camera + renegotiates
     * (async), so the UI shows a camera-button spinner + a self-tile "Starting
     * camera…" hint while [Starting]:
     *   * [Off]      — no camera-on in flight (default; also after a failure).
     *   * [Starting] — the camera acquire + [addLocalVideo] renegotiation are
     *                  in flight.
     *   * [Live]     — the local camera is captured, SFrame-sealed, and added.
     *   * [Failed]   — no camera / capture / SFrame-arm failure; the caller
     *                  reverts the toggle. The call stays up audio-only
     *                  (invariant #4 — no half-added plaintext sender).
     */
    public enum class VideoPublishState { Off, Starting, Live, Failed }

    private val mutableState: MutableStateFlow<WebCallState> = MutableStateFlow(WebCallState.Idle)
    override val state: StateFlow<WebCallState> = mutableState.asStateFlow()

    private val mutableEndReason: MutableStateFlow<WebCallEndReason?> = MutableStateFlow(null)
    public val endReason: StateFlow<WebCallEndReason?> = mutableEndReason.asStateFlow()

    private var isOfferer: Boolean = false

    /** The wake each outbound frame asks for; remembers whether this call
     *  ever connected on this device. */
    private val wakes = OneToOneCallWakes()

    // ---- Perfect-negotiation state (piece B2b: 1×1 mid-call camera-on) ----
    // The 1×1 P2P path is SYMMETRIC, so glare (both peers renegotiating at
    // once) is possible. Canonical perfect-negotiation: the OFFERER (caller) is
    // the IMPOLITE peer, the ANSWERER (callee) is POLITE (`polite = !isOfferer`)
    // — the polite peer yields on a collision. Unlike the browser (implicit
    // rollback inside `setRemoteDescription`), native libwebrtc needs an
    // EXPLICIT rollback ([WebRtcPeerConnection.rollbackLocalDescription]), so
    // the polite peer rolls its own in-flight offer back before answering a
    // colliding one. Every flag here is read / written only on [scope]
    // (single-threaded), so no synchronisation is needed.
    //
    //   * makingOffer                  — our own reneg offer is in flight
    //                                    (createOffer .. its answer arrives).
    //   * isSettingRemoteAnswerPending — guards the reneg-answer apply window
    //                                    (part of the glare `readyForOffer`
    //                                    predicate, mirroring the web).
    //   * negotiationReady             — false during the manual initial
    //                                    offer/answer; armed once that exchange
    //                                    lands, from when on a mid-call
    //                                    renegotiation may fire.
    //   * pendingRenegotiation         — a camera-on / rolled-back offer that
    //                                    must be re-driven once the current
    //                                    exchange settles (native has no
    //                                    `onnegotiationneeded` auto re-fire).
    private var makingOffer = false
    private var isSettingRemoteAnswerPending = false
    private var negotiationReady = false
    private var pendingRenegotiation = false

    /**
     * The media flag the deferred [pendingRenegotiation] must carry when it is
     * finally driven. Kept beside the flag rather than derived at send time:
     * the reason the offer was queued (the camera going on, or off) is known
     * only at the moment it was queued, and re-deriving it later from whatever
     * the camera happens to be doing by then is how an offer starts lying.
     */
    private var pendingRenegotiationVideo = true

    /** In-flight guard for [addLocalVideo] (no double camera acquire). */
    private var addingVideo = false

    /**
     * Stored remote SDP offer between `Ringing` →
     * `Connecting`. [acceptCall] reads it to build the
     * answer.
     */
    private var pendingRemoteOfferSdp: String? = null

    /**
     * Remote ICE candidates that arrived before the remote description was
     * set: while ringing (no `PeerConnection` until [acceptCall]) and in the
     * window between an offer / answer arriving and `setRemoteDescription`
     * completing. Released in arrival order right after the remote
     * description lands ([applyBufferedRemoteCandidates]); dropping them
     * instead can leave ICE with no reachable path.
     */
    private val remoteCandidates = RemoteCandidateBuffer()

    /**
     * The WebRTC pod. Lazily constructed inside [dial] /
     * [acceptCall] so an `Idle` session pays nothing.
     */
    private var rtc: WebRtcPeerConnection? = null

    /**
     * G3-2-video-ui — local camera capture lifecycle. Held
     * for the duration of a video-enabled call so the
     * `CameraVideoCapturer` + `VideoSource` pair survives a
     * Compose recompose. Released in [endLocal] / [endRemote].
     */
    private var localCameraCapture: CameraVideoCapture? = null

    /**
     * G3-2-video-ui — observable for the Compose preview
     * overlay. Compose layer collects this via `collectAsState`
     * and renders `LocalVideoPreviewView(track = ..., eglContext = ...)`
     * when non-null. Set in [startLocalCameraCapture];
     * cleared in [teardownMediaPipeline].
     */
    private val mutableLocalVideoTrack: MutableStateFlow<org.webrtc.VideoTrack?> =
        MutableStateFlow(null)
    public val localVideoTrack: StateFlow<org.webrtc.VideoTrack?> =
        mutableLocalVideoTrack.asStateFlow()

    /**
     * G3-2-remote-video — the remote participant's incoming
     * video track, surfaced from `WebRtcPeerConnection`'s
     * `onRemoteVideoTrack` delegate callback. `null` until
     * the remote adds a camera track (or after the call
     * ends). On the 1×1 direct-PeerConnection path there is
     * exactly one remote, so this single handle is
     * unambiguous; the N-way SFU path (per-participant
     * MSID→leaf mapping) is a deferred follow-up that will
     * replace this with a `Map<participantId, VideoTrack>`
     * keyed on the stream id the SFU stamps.
     */
    private val mutableRemoteVideoTrack: MutableStateFlow<org.webrtc.VideoTrack?> =
        MutableStateFlow(null)
    public val remoteVideoTrack: StateFlow<org.webrtc.VideoTrack?> =
        mutableRemoteVideoTrack.asStateFlow()

    /**
     * ADR-0083 section 4 — the picture of the other person's shared screen:
     * a remote video track in a stream whose id ends in `~screen`, kept apart
     * from their camera. `null` when nobody shares, and after the call.
     */
    private val mutableRemoteScreenTrack: MutableStateFlow<org.webrtc.VideoTrack?> =
        MutableStateFlow(null)
    public val remoteScreenTrack: StateFlow<org.webrtc.VideoTrack?> =
        mutableRemoteScreenTrack.asStateFlow()

    /** The call's current screen share from the shared fold, or `null`. */
    private val mutableScreenShare: MutableStateFlow<ScreenShareView?> = MutableStateFlow(null)
    public val screenShare: StateFlow<ScreenShareView?> = mutableScreenShare.asStateFlow()

    /** The fold, made on the first share state. Main thread only. */
    private var screenShareReceiver: ScreenShareReceiver? = null

    /**
     * A share state from the other person, verified against the
     * conversation's MLS sender leaf (`verifyCallScreenShare` with
     * `callScreenShareOneToOnePeerId()`). Folded; the screen follows
     * [screenShare]. Ignored once the call is over.
     */
    override fun applyScreenShare(share: VerifiedCallScreenShare) {
        if (mutableState.value == WebCallState.Done) return
        val receiver = screenShareReceiver ?: run {
            val ownKey = ownSignatureKey ?: return
            val fold = try {
                NativeScreenShareFold(ownKey)
            } catch (_: Throwable) {
                return
            }
            ScreenShareReceiver(fold) { mutableScreenShare.value = it }.also { screenShareReceiver = it }
        }
        receiver.receive(share)
    }

    /**
     * G3-2-call-controls — observable local mic / camera enabled
     * state, so the in-call control buttons reflect (and drive)
     * the real WebRTC track enabled flags. `micEnabled` starts
     * true; `cameraEnabled` starts false and flips true when the
     * camera pipeline arms on a video call. `CallUiState.driveFrom`
     * maps these onto the local participant's `micOn` / `camOn`.
     */
    private val mutableMicEnabled: MutableStateFlow<Boolean> = MutableStateFlow(true)
    public val micEnabled: StateFlow<Boolean> = mutableMicEnabled.asStateFlow()
    private val mutableCameraEnabled: MutableStateFlow<Boolean> = MutableStateFlow(false)
    public val cameraEnabled: StateFlow<Boolean> = mutableCameraEnabled.asStateFlow()

    /** ADR-0032 piece B2a — mid-call camera-on lifecycle (see
     *  [VideoPublishState]). [addLocalVideo] sets [VideoPublishState.Starting]
     *  before the async camera acquire, [VideoPublishState.Live] on success,
     *  [VideoPublishState.Failed] on any thrown error, and
     *  [teardownMediaPipeline] resets it to [VideoPublishState.Off].
     *  `CallUiState.driveFrom` maps it onto the shared UI surface the
     *  `CallScreen` reads for the camera-button spinner + the self-tile hint. */
    private val mutableVideoPublishState: MutableStateFlow<VideoPublishState> =
        MutableStateFlow(VideoPublishState.Off)
    public val videoPublishState: StateFlow<VideoPublishState> =
        mutableVideoPublishState.asStateFlow()

    /** G3-2-e2ee-active — `true` once [setCallKey] has armed the
     *  SFrame seed (the call's media is end-to-end encrypted).
     *  `CallUiState.driveFrom` maps this onto the E2EE pill so
     *  it shows the green lock only when keying is real. The
     *  short comparable fingerprint is a deferred follow-up
     *  (needs a canonical cross-platform hash of the seed). */
    private val mutableE2eeActive: MutableStateFlow<Boolean> = MutableStateFlow(false)
    public val e2eeActive: StateFlow<Boolean> = mutableE2eeActive.asStateFlow()

    /** Canonical call safety number — `callSafetyNumber(seed)` over the
     *  shared per-call seed, set alongside [e2eeActive] in [setCallKey].
     *  The same digits every participant derives; `CallUiState.driveFrom`
     *  maps it onto the E2EE pill for an out-of-band man-in-the-middle
     *  check. `null` until the key is armed (or if the derivation fails —
     *  display-only, never fatal). */
    private val mutableSafetyNumber: MutableStateFlow<String?> = MutableStateFlow(null)
    public val safetyNumber: StateFlow<String?> = mutableSafetyNumber.asStateFlow()

    /** G3-2-conn-quality — live connection stats polled from the
     *  pod's `RTCStatsReport` every couple of seconds while
     *  connected. `rttMs` null until ICE settles; `connectionRelayed`
     *  true when the active pair goes through a TURN relay.
     *  `CallUiState.driveFrom` maps these onto the header
     *  quality-dot + RTT/connection-type popover. */
    private val mutableRttMs: MutableStateFlow<Int?> = MutableStateFlow(null)
    public val rttMs: StateFlow<Int?> = mutableRttMs.asStateFlow()
    private val mutableConnectionRelayed: MutableStateFlow<Boolean> = MutableStateFlow(false)
    public val connectionRelayed: StateFlow<Boolean> = mutableConnectionRelayed.asStateFlow()

    /** G3-2-conn-quality iter2 — receive / send bitrate in **bits
     *  per second** (the popover's down/up lines), derived from the
     *  candidate-pair's cumulative byte counters diffed across two
     *  poll samples against the wall clock. null until a second
     *  sample lands (a single sample carries no rate). */
    private val mutableDownBps: MutableStateFlow<Int?> = MutableStateFlow(null)
    public val downBps: StateFlow<Int?> = mutableDownBps.asStateFlow()
    private val mutableUpBps: MutableStateFlow<Int?> = MutableStateFlow(null)
    public val upBps: StateFlow<Int?> = mutableUpBps.asStateFlow()

    /** Previous (bytesReceived, bytesSent, monotonic nanos) used to
     *  derive [downBps] / [upBps]. Reset in [teardownMediaPipeline]
     *  so a fresh call starts clean. */
    private var prevStatsBytes: Triple<Long, Long, Long>? = null

    /** Background poll loop driving [rttMs] / [connectionRelayed] /
     *  [downBps] / [upBps]. Started on the first Connected
     *  transition; cancelled in [teardownMediaPipeline]. */
    private var statsJob: kotlinx.coroutines.Job? = null

    /** What the receivers report, logged with every stats sample and as a
     *  total in [teardownMediaPipeline] (ADR-0067 step 1, as the group call
     *  does). Touched only on the session's scope. */
    private val inboundCounters = InboundCounterLog()

    /**
     * G3-sframe-native N-6 — per-call MLS exporter seed and
     * local participant leaf index. Both must be set BEFORE
     * [dial] / [acceptCall]; missing seed at attach time
     * aborts the call (security invariant #4).
     */
    private var callKeySeed: ByteArray? = null
    private var localLeaf: Int? = null

    /**
     * Inject the per-call SFrame keying material. Caller MUST
     * run this AFTER the welcome-ferry / KP-forward dispatcher
     * has resolved (for joiners) or AFTER `Group.create` on
     * the 1×1-from-chat path (for the originator), and BEFORE
     * [dial] / [acceptCall].
     *
     * `seed` is exactly 32 bytes (`Group.export_secret
     * ("lakko/call/v1", call_id, 32)`); `localLeaf` is this
     * device's leaf index in the call's MLS group.
     */
    override fun setCallKey(seed: ByteArray, localLeaf: Int) {
        this.callKeySeed = seed
        this.localLeaf = localLeaf
        // G3-2-e2ee-active — SFrame seed present → E2EE keying
        // armed; the UI pill mirrors this.
        mutableE2eeActive.value = true
        // The safety number is a canonical hash of the same seed — shown
        // in the pill for an out-of-band MITM check. Display-only: a
        // derivation failure (wrong-length seed, which the exporter never
        // emits) leaves it null and never breaks the call.
        mutableSafetyNumber.value =
            try {
                callSafetyNumber(seed)
            } catch (e: Exception) {
                null
            }
    }

    /**
     * G3-2-turn — TURN-credential client. Injected by the
     * call-UI layer via [setTurnClient]; nullable so dev
     * builds without a deployed coturn cluster still work
     * (fail-soft: no ICE server, host candidates only).
     */
    private var turnClient: TurnCredentialClient? = null

    public fun setTurnClient(client: TurnCredentialClient?) {
        this.turnClient = client
    }

    /**
     * Session-owned scope. Every coroutine launched from
     * delegate callbacks (which fire on libwebrtc's worker
     * threads) hops into [Dispatchers.Main] via this scope
     * so [StateFlow] mutations stay on the main thread.
     * [endLocal] / [endRemote] cancel it before nulling the
     * PeerConnection.
     */
    private val scope: CoroutineScope =
        CoroutineScope(SupervisorJob() + Dispatchers.Main)

    public companion object {
        /**
         * A CONNECTED call that has received nothing for this long is over
         * ([MediaWatchdog]). Chosen, not measured: several times any
         * renegotiation gap, and short enough that a call whose peer has gone
         * does not sit there counting time.
         */
        public const val MEDIA_SILENCE_TIMEOUT_MS: Long = 30_000

        /**
         * DISCONNECTED is often transient (a Wi-Fi roam, an LTE handover); the
         * call ends only if it has not recovered after this. The same window
         * the web uses. FAILED still ends the call at once.
         */
        public const val ICE_DISCONNECTED_GRACE_MS: Long = 10_000

        /** Generate a fresh 16-byte call_id. */
        public fun mintCallId(): ByteArray {
            val bytes = ByteArray(16)
            SecureRandom().nextBytes(bytes)
            return bytes
        }

        /** Pull the embedded call_id from any
         *  [FfiCallSignalingBody] variant. */
        public fun callIdOf(body: FfiCallSignalingBody): ByteArray = when (body) {
            is FfiCallSignalingBody.Offer -> body.callId
            is FfiCallSignalingBody.Answer -> body.callId
            is FfiCallSignalingBody.IceCandidate -> body.callId
            is FfiCallSignalingBody.End -> body.callId
            is FfiCallSignalingBody.ScreenShare -> body.callId
            is FfiCallSignalingBody.GroupStart -> body.callId
            is FfiCallSignalingBody.GroupJoin -> body.callId
            is FfiCallSignalingBody.GroupLeave -> body.callId
        }
    }

    // ----------------------------------------------------------------
    // Outbound
    // ----------------------------------------------------------------

    /**
     * Outbound side: dial. Spins up [WebRtcPeerConnection],
     * attaches a local audio track, generates a real SDP
     * offer and ships it via the existing signaling pipe.
     */
    override fun dial(audio: Boolean, video: Boolean) {
        if (mutableState.value != WebCallState.Idle) return
        isOfferer = true
        mutableState.value = WebCallState.Outgoing

        // G3-2-turn — fetch the TURN credential first so the
        // RTCPeerConnection sees the relay candidate in its
        // initial configuration (same rationale as iOS).
        scope.launch {
            val iceServers = resolveIceServers()
            // The credential fetch can take seconds, and the person may have
            // pressed End meanwhile. Nothing may start for a call that is no
            // longer being placed: the camera opened below would have nobody
            // left to close it.
            if (mutableState.value != WebCallState.Outgoing) return@launch
            val rtc = WebRtcPeerConnection(applicationContext, iceServers = iceServers)
            if (!rtc.isReady) {
                android.util.Log.w("CallSession", "dial: PeerConnection allocation failed")
                endLocal(WebCallEndReason.Error)
                return@launch
            }
            installDelegate(rtc)
            rtc.addLocalAudioTrack(enabled = audio)

            // G3-2-video-ui — local video track + camera
            // capture. MUST land BEFORE createOffer: the SDP
            // build emits the m=video line based on what
            // PeerConnection already has attached; adding the
            // track later would force a renegotiation we do
            // not drive.
            if (video) {
                startLocalCameraCapture(rtc)
            }

            if (!attachSFrame(rtc)) {
                android.util.Log.w(
                    "CallSession",
                    "dial: SFrame attach failed — refusing per security invariant #4",
                )
                // Not this session's connection yet, so the teardown below
                // would not find it: close it here.
                rtc.close()
                endLocal(WebCallEndReason.Error)
                return@launch
            }
            this@CallSession.rtc = rtc

            try {
                val offerSdp = rtc.createOffer(audio = audio, video = video)
                val bytes = composeCallSignaling(
                    FfiCallSignalingBody.Offer(
                        callId = callId,
                        sdp = offerSdp,
                        createdAtMs = System.currentTimeMillis().toULong(),
                        media = FfiCallMediaFlags(audio = audio, video = video),
                    ),
                )
                onOutboundSignaling(bytes, wakes.dialOffer())
            } catch (cause: Throwable) {
                android.util.Log.w("CallSession", "dial offer-build failed", cause)
                endLocal(WebCallEndReason.Error)
            }
        }
    }

    /**
     * The ICE servers of this call, Lakko's own only (`IceServers.kt`): the
     * TURN entry minted via [TurnCredentialClient.fetchOrNull] and STUN on
     * its host, or none at all (host candidates only) when [turnClient] is
     * unset or the fetch returns null.
     */
    private suspend fun resolveIceServers(): List<PeerConnection.IceServer> {
        val servers = IceServers.forTurn(turnClient?.fetchOrNull())
        android.util.Log.i("CallSession", "call ice servers: ${IceServers.logLine(servers)}")
        return IceServers.toWebRtc(servers)
    }

    /** G3-2-call-controls — mute / unmute the local mic. Drives
     *  the real `AudioTrack.setEnabled` via the pod and publishes
     *  the new state so the in-call control reflects it. */
    public fun setMicrophoneEnabled(on: Boolean) {
        rtc?.setMicrophoneEnabled(on)
        mutableMicEnabled.value = on
    }

    /** G3-2-call-controls — start / stop the local camera track
     *  (`VideoTrack.setEnabled` via the pod; no SDP
     *  renegotiation) and publish the new state. */
    public fun setCameraEnabled(on: Boolean) {
        rtc?.setCameraEnabled(on)
        mutableCameraEnabled.value = on
    }

    /**
     * Stop sending the camera AND tell the peer — `docs/ux/calls.md` section
     * 4.1: "Turning the camera off again returns the call to the portrait",
     * which has to be true on both screens or the other person is left looking
     * at a still frame of someone who has put their camera away.
     *
     * The frames stop first, so the local preview and the wire go quiet at
     * once; then the video transceiver moves to `recvOnly` and one
     * renegotiation carries it. The peer's libwebrtc fires its remove-track
     * callback off that, their `remoteVideoTrack` clears, and their screen
     * falls back to the portrait by itself.
     *
     * The sender is deliberately kept. Removing the track would tell the peer
     * just as well and would drop the SFrame encryptor with it, so the next
     * camera-on would arm a fresh one for the same `(leaf, stream)` pair with
     * the counter restarted over a key already used — the reuse ADR-0035 exists
     * to prevent. Keeping it also makes the next camera-on the cheap branch of
     * [addLocalVideo]: re-enable, no capture, no second arm.
     *
     * Falls back to the plain disable when there is no video transceiver yet or
     * the platform refuses the direction change. That costs the peer a frozen
     * frame — the behaviour before this existed — and never the call.
     */
    public suspend fun stopSendingVideo() {
        val connection = rtc
        if (connection == null) {
            mutableCameraEnabled.value = false
            return
        }
        connection.setCameraEnabled(false)
        mutableCameraEnabled.value = false
        if (connection.setLocalVideoSending(false)) {
            driveRenegotiationOffer(video = false)
        }
    }

    /**
     * `docs/ux/calls.md` section 4.1 — flip to the other camera, driven by the
     * small button in the self-view's corner rather than by a dropdown entry
     * (which existed, and did nothing).
     *
     * A no-op when no camera is being captured. The swap keeps the same video
     * track and sender, so nothing is renegotiated and the SFrame encryptor
     * armed on that sender stays armed — a flip can never drop the call out of
     * its sealed state.
     */
    public fun switchCamera() {
        val capture = localCameraCapture ?: return
        capture.switchCamera { switched, isFrontFacing ->
            if (switched) {
                mutableCameraIsFrontFacing.value = isFrontFacing
            }
        }
    }

    /** The camera chosen in the call's Settings sheet, or `null` for the
     *  default. Held for the call, so a camera started later opens on it. */
    private var preferredCamera: String? = null

    /**
     * Use the camera named [cameraName] for this call, or the default one for
     * `null` (`docs/ux/calls.md` section 4.1, the Settings sheet).
     *
     * Applies at once when the camera is being captured, by the same swap as
     * [switchCamera]: the track and the sender stay, so nothing is
     * renegotiated and the SFrame encryptor stays armed. Otherwise it is kept
     * for when the camera is turned on.
     */
    public fun selectCamera(cameraName: String?) {
        preferredCamera = cameraName
        val capture = localCameraCapture ?: return
        val target = cameraName ?: CameraVideoCapture.defaultCameraName(applicationContext) ?: return
        capture.switchTo(target) { switched, isFrontFacing ->
            if (switched) {
                mutableCameraIsFrontFacing.value = isFrontFacing
            }
        }
    }

    private val mutableCameraIsFrontFacing: MutableStateFlow<Boolean> = MutableStateFlow(true)

    /**
     * `true` while the captured camera faces the user. The self-view is
     * mirrored only then: a person expects to see themselves as in a mirror,
     * and expects the back camera to show the world the right way round.
     */
    public val cameraIsFrontFacing: StateFlow<Boolean> = mutableCameraIsFrontFacing.asStateFlow()

    /**
     * ADR-0032 piece B2b — turn the camera ON mid-call when the 1×1 call
     * started audio-only. Mirror of the web `CallSession.addLocalVideo`.
     *
     * When there is no video sender yet this ACQUIRES the camera (front /
     * 720p / 30 via [CameraVideoCapture]), adds the track to the peer
     * connection, arms SFrame on the NEW video sender, and drives a
     * renegotiation offer directly ([driveRenegotiationOffer]).
     *
     * The native 1×1 path is SFrame-sealed (unlike the web 1×1, which is
     * plain DTLS-SRTP): [attachSFrame] arms an encryptor on every sender at
     * connect using the [setCallKey] seed. The mid-call video sender must be
     * sealed the SAME way, or (a) the peer's SFrame decryptor cannot open the
     * video, and (b) an unsealed m-line would leak plaintext media — a break
     * of invariant #4. So this arms the video sender with the same
     * `callKeySeed` + `localLeaf` [attachSFrame] uses. Idempotent +
     * re-entrant-safe:
     *   * a live video sender already exists (a video call, or a prior
     *     addLocalVideo) -> just re-enable its track; no capture, no reneg;
     *   * a capture is already in flight ([addingVideo]) -> no-op.
     *
     * Fail-closed (invariant #4): on a no-camera / capture failure, or if the
     * SFrame keying material is unavailable, or the SFrame arm on the new
     * video sender fails, the just-added sender (if any) is removed and the
     * capture torn down — NEVER a half-added, plaintext video m-line — and it
     * THROWS, so the caller reverts the camera toggle; the call stays up
     * audio-only. Because the UI's camera state is driven from
     * [cameraEnabled], the flag is only flipped true on success, so a failure
     * leaves the button reverted automatically.
     *
     * Runs on [scope]'s dispatcher via the caller's coroutine; the
     * perfect-negotiation flags it touches need no synchronisation.
     *
     * @throws WebRtcException on a no-camera / capture / SFrame-arm failure.
     */
    public suspend fun addLocalVideo() {
        val connection = rtc ?: return
        // 1. A live video sender already exists -> just re-enable it. No
        //    capture, no renegotiation (the track was negotiated already, and
        //    its SFrame encryptor is still armed).
        if (connection.hasLiveLocalVideoSender()) {
            connection.setCameraEnabled(true)
            mutableLocalVideoTrack.value = connection.currentLocalVideoTrack()
            mutableCameraEnabled.value = true
            mutableVideoPublishState.value = VideoPublishState.Live
            // The sender, its track and its SFrame encryptor are all still the
            // ones negotiated the first time, so nothing is re-armed and the
            // encryptor's counter carries on where it left off. What DOES need
            // saying is that video is flowing again, because [stopSendingVideo]
            // parked the transceiver at `recvOnly` and the peer dropped its
            // receiver; without this their screen would never come back.
            if (connection.setLocalVideoSending(true)) {
                driveRenegotiationOffer(video = true)
                // The answer drives the watchdog and the probe; a re-enable that
                // needed no renegotiation is already negotiated as sending, so
                // it keys and measures here instead.
            } else {
                startKeyframeWatchdog()
                diagnoseMidCallVideo("camera re-enabled")
            }
            return
        }
        // 2. A capture is already in flight -> do not double-acquire.
        if (addingVideo) return
        // No SFrame keying -> cannot seal the video, so cannot publish it
        // (invariant #4). The 1×1 call already refused at dial/accept if the
        // seed was unset, so this is defence-in-depth; throw so the toggle
        // reverts.
        val seed = callKeySeed
        val leaf = localLeaf
        if (seed == null || leaf == null ||
            !me.lakko.sframe.SFrameAvailability.isCallingAvailable
        ) {
            mutableVideoPublishState.value = VideoPublishState.Failed
            throw WebRtcException(
                "SFrame keying unavailable — refusing mid-call video (invariant #4)",
            )
        }
        addingVideo = true
        // ADR-0032 piece B2a — the camera acquire + renegotiation are now in
        // flight; the UI shows the camera-button spinner + the self-tile
        // "Starting camera…" hint until this resolves to Live / Failed.
        mutableVideoPublishState.value = VideoPublishState.Starting
        try {
            // A sealed video sender whose track ended by itself: its capture
            // is dead. Release it before acquiring the camera again; the
            // sender itself is kept and reused in step 5.
            if (connection.hasSealedLocalVideoSender()) {
                localCameraCapture?.let { stale ->
                    stale.stop()
                    stale.dispose()
                }
                localCameraCapture = null
            }
            // 3. Capture the camera. On any failure -> throw; no sender has
            //    been added yet, so there is nothing half-added to clean up.
            if (!CameraVideoCapture.hasUsableCamera(applicationContext)) {
                throw WebRtcException("no camera available")
            }
            val source = WebRtcPeerConnection.newVideoSource(applicationContext)
            val eglContext = WebRtcPeerConnection.sharedEglContext()
            if (source == null || eglContext == null) {
                throw WebRtcException("could not create a video source")
            }
            val capture = CameraVideoCapture.create(applicationContext, source, eglContext, preferredCamera)
                ?: throw WebRtcException("could not create the camera capture")
            mutableCameraIsFrontFacing.value = capture.startsFrontFacing
            try {
                capture.start()
            } catch (cause: CameraVideoCapture.CaptureError) {
                capture.dispose()
                throw WebRtcException("camera start failed: ${cause.message}")
            }

            // 4. Re-read the pc AFTER the (blocking) capture start — a teardown
            //    may have interleaved. If it went away, abandon cleanly (not a
            //    user error — the call is going away).
            val connection2 = rtc
            if (connection2 == null) {
                capture.stop()
                capture.dispose()
                // The call went away mid-acquire — teardown already reset (or
                // is about to reset) the state; do not surface a failure.
                mutableVideoPublishState.value = VideoPublishState.Off
                return
            }

            // A sealed video sender whose track ended by itself is still on
            // the connection. One sender per stream: put the fresh track on
            // THAT sender, keeping its SFrame encryptor and counter. A second
            // sender would seal the same (leaf, stream) KID under a second
            // encryptor (ADR-0035, ADR-0069).
            if (connection2.hasSealedLocalVideoSender()) {
                if (!connection2.replaceLocalVideoTrack(source)) {
                    capture.stop()
                    capture.dispose()
                    throw WebRtcException("could not put the camera on the existing video sender")
                }
                localCameraCapture = capture
                mutableLocalVideoTrack.value = connection2.currentLocalVideoTrack()
                mutableCameraEnabled.value = true
                mutableVideoPublishState.value = VideoPublishState.Live
                // The sender may be parked at `recvOnly` by an earlier
                // camera-off; say that video flows again, as the re-enable
                // path above does.
                if (connection2.setLocalVideoSending(true)) {
                    driveRenegotiationOffer(video = true)
                } else {
                    startKeyframeWatchdog()
                    diagnoseMidCallVideo("camera track replaced")
                }
                return
            }

            // 5. Add the video track, then arm SFrame on ONLY the NEW video
            //    sender (the audio encryptor from attachSFrame is left
            //    untouched). Fail-closed: if the arm fails, remove the
            //    just-added sender + tear the capture down so a plaintext
            //    m-line is NEVER offered to the peer (invariant #4) — the call
            //    stays audio-only. Mirrors the SFU publishVideo path.
            connection2.addLocalVideoTrack(source)
            if (!connection2.armSFrameEncryptorForVideoSender(seed, leaf)) {
                connection2.removeLocalVideoTrack()
                capture.stop()
                capture.dispose()
                throw WebRtcException("SFrame arm on the video sender failed (invariant #4)")
            }
            localCameraCapture = capture
            mutableLocalVideoTrack.value = connection2.currentLocalVideoTrack()
            mutableCameraEnabled.value = true
            // Live: the camera is published + sealed; the spinner clears (the
            // UI shows it only while Starting).
            mutableVideoPublishState.value = VideoPublishState.Live

            // 6. Renegotiate so the peer learns about the new (sealed) video
            //    m-line. Stated explicitly rather than left to the default: the
            //    default carries whatever a PREVIOUS camera-off queued, and an
            //    offer that adds a video sender while announcing `video = false`
            //    is the same lie in the other direction.
            driveRenegotiationOffer(video = true)
            // No probe and no keyframe here: the sender exists but is not
            // negotiated until the peer answers, and both belong there.
        } catch (cause: Throwable) {
            // Fail-safe (invariant #4): any thrown error leaves the call up
            // audio-only. Failed drives the caller's toggle revert (the button
            // reverts automatically — cameraEnabled stays false). Rethrow so
            // the caller can log / surface it.
            mutableVideoPublishState.value = VideoPublishState.Failed
            throw cause
        } finally {
            addingVideo = false
        }
    }

    /**
     * Say, on the console, why a mid-call camera-on did or did not reach the
     * other phone — see [MidCallVideoDiagnosis].
     *
     * Live, Android to iOS, the caller's own screen filled with their camera
     * while iOS attached the track and waited for a first frame that never
     * arrived. The three causes that produce exactly those two observations —
     * an unsealed sender, a transceiver not negotiated as sending, and a sender
     * that never encodes — are indistinguishable from either end, so this reads
     * all three and names the one that fired.
     *
     * Deliberately delayed: `framesEncoded` and `bytesSent` are zero for a
     * second or two after any camera-on, and a probe that fires immediately
     * would report NOT_ENCODING for a call that is perfectly healthy. It runs
     * detached from the caller's coroutine so a slow stats callback never holds
     * up the camera button, and it logs and nothing else — a diagnosis must not
     * be able to change the call it is diagnosing.
     */
    private fun diagnoseMidCallVideo(label: String = "mid-call camera-on") {
        scope.launch {
            delay(MID_CALL_VIDEO_PROBE_DELAY_MS)
            val connection = rtc ?: return@launch
            val facts = try {
                connection.sampleLocalVideoSendFacts()
            } catch (cause: Throwable) {
                android.util.Log.w("CallSession", "$label: probe failed", cause)
                return@launch
            }
            android.util.Log.i("CallSession", midCallVideoReport(facts, label))
        }
        // A second read, late enough that `keyFramesEncoded` and `pliCount`
        // describe a sender that has actually been sending. The first read can
        // only say whether the sender started; these two counters mean nothing
        // until some traffic has gone by, and the earlier single read took them
        // before the renegotiation had even completed.
        scope.launch {
            delay(MID_CALL_VIDEO_LATE_PROBE_DELAY_MS)
            val connection = rtc ?: return@launch
            val facts = runCatching { connection.sampleLocalVideoSendFacts() }.getOrNull()
                ?: return@launch
            android.util.Log.i("CallSession", midCallVideoReport(facts, "$label late"))
        }
    }

    /** Outbound or active call teardown. */
    override fun hangUp(reason: WebCallEndReason) {
        endLocal(reason)
    }

    /** Inbound side: accept the ringing call. Builds the SDP
     *  answer against the stored remote offer + ships it.
     *
     *  `withVideo` mirrors the offerer's `dial(video=)`: the
     *  answerer can choose to publish video or stay audio-only
     *  while accepting a video call. Defaults to `false` —
     *  the UI's "accept with camera" toggle threads its value
     *  through to here. */
    override fun acceptCall(withVideo: Boolean) {
        if (mutableState.value != WebCallState.Ringing || isOfferer) return
        // One answer per call: the state stays Ringing while the credential is
        // fetched, and a second accept in that time would build a second peer
        // connection and open the camera twice.
        if (accepting) return
        val offerSdp = pendingRemoteOfferSdp
        if (offerSdp == null) {
            android.util.Log.w("CallSession", "acceptCall: pendingRemoteOfferSdp missing — race?")
            endLocal(WebCallEndReason.Error)
            return
        }
        accepting = true

        scope.launch {
            val iceServers = resolveIceServers()
            // The caller may have hung up, or the ring run out, while the
            // credential was fetched: the call is then over and must neither
            // open the camera nor come back as connecting.
            if (mutableState.value != WebCallState.Ringing) return@launch
            val rtc = WebRtcPeerConnection(applicationContext, iceServers = iceServers)
            if (!rtc.isReady) {
                android.util.Log.w("CallSession", "acceptCall: PeerConnection allocation failed")
                endLocal(WebCallEndReason.Error)
                return@launch
            }
            installDelegate(rtc)
            rtc.addLocalAudioTrack(enabled = true)

            // G3-2-video-ui — symmetric video wire-up on
            // accept path. Same pre-SDP-build ordering as
            // dial().
            if (withVideo) {
                startLocalCameraCapture(rtc)
            }

            if (!attachSFrame(rtc)) {
                android.util.Log.w(
                    "CallSession",
                    "acceptCall: SFrame attach failed — refusing per security invariant #4",
                )
                // Not this session's connection yet, so the teardown below
                // would not find it: close it here.
                rtc.close()
                endLocal(WebCallEndReason.Error)
                return@launch
            }
            this@CallSession.rtc = rtc

            mutableState.value = WebCallState.Connecting

            try {
                val answerSdp = rtc.createAnswer(remoteOfferSdp = offerSdp)
                // The answer fixed each sender's codec: tell the video
                // encryptor, so it leaves that codec's frame header clear
                // (ADR-0071). See [syncSenderCodecsAfterNegotiation].
                syncSenderCodecsAfterNegotiation(rtc)
                // The remote offer is now the remote description: the
                // candidates the caller trickled while this call rang can go in.
                applyBufferedRemoteCandidates(rtc)
                val bytes = composeCallSignaling(
                    FfiCallSignalingBody.Answer(
                        callId = callId,
                        sdp = answerSdp,
                    ),
                )
                onOutboundSignaling(bytes, wakes.inCall())
                // ADR-0032 piece B2b — the initial answer is sent + our local
                // description is set, so the initial negotiation is applied on
                // our side: arm renegotiation for a future mid-call camera-on,
                // and drive one that queued before this point.
                negotiationReady = true
                if (pendingRenegotiation) driveRenegotiationOffer()
                // Don't synthesise Connected here — wait for
                // the real PeerConnection state transition.
            } catch (cause: Throwable) {
                android.util.Log.w("CallSession", "accept compose failed", cause)
                endLocal(WebCallEndReason.Error)
            }
        }
    }

    /** [acceptCall] has been taken for this call. Main thread only. */
    private var accepting = false

    /** Inbound side: decline. */
    override fun declineCall() {
        endLocal(WebCallEndReason.Declined)
    }

    // ----------------------------------------------------------------
    // Inbound
    // ----------------------------------------------------------------

    /** Route a parsed signaling payload into the state
     *  machine. Caller has already verified call_id matches
     *  this session. */
    override fun applyIncomingSignaling(body: FfiCallSignalingBody) {
        when (body) {
            is FfiCallSignalingBody.Offer -> {
                if (mutableState.value == WebCallState.Idle) {
                    // Fresh inbound call — we're the answerer. Store the offer
                    // + ring; the PeerConnection is built later in [acceptCall].
                    isOfferer = false
                    pendingRemoteOfferSdp = body.sdp
                    mutableState.value = WebCallState.Ringing
                } else {
                    // ADR-0032 piece B2b — a mid-call renegotiation offer (the
                    // peer turned their camera on). NOT a fresh call: apply it
                    // with the perfect-negotiation glare guard + explicit
                    // rollback, keeping the call up. `rtc == null` (answerer
                    // still Ringing, no PC yet) drops it.
                    if (this.rtc == null) return
                    scope.launch { applyRenegotiationOffer(body.sdp) }
                }
            }
            is FfiCallSignalingBody.Answer -> {
                val rtc = this.rtc ?: return
                if (mutableState.value == WebCallState.Outgoing) {
                    // Initial answer to our dial.
                    mutableState.value = WebCallState.Connecting
                    scope.launch {
                        try {
                            rtc.applyRemoteAnswer(body.sdp)
                            syncSenderCodecsAfterNegotiation(rtc)
                            applyBufferedRemoteCandidates(rtc)
                            // ADR-0032 piece B2b — arm renegotiation for future
                            // local track changes; drive a camera-on that was
                            // queued before the initial answer landed.
                            negotiationReady = true
                            if (pendingRenegotiation) driveRenegotiationOffer()
                            // A call DIALLED with video arms its sender at
                            // connect; print the same facts the mid-call arm
                            // prints so the two are compared on measurements.
                            if (mutableCameraEnabled.value) {
                                diagnoseMidCallVideo("call-start-with-video")
                            }
                        } catch (cause: Throwable) {
                            android.util.Log.w("CallSession", "applyRemoteAnswer failed", cause)
                            endLocal(WebCallEndReason.Error)
                        }
                    }
                } else {
                    // ADR-0032 piece B2b — a mid-call renegotiation answer (to a
                    // reneg offer WE sent). Plain apply, guarded by
                    // [isSettingRemoteAnswerPending] for the glare predicate.
                    scope.launch { applyRenegotiationAnswer(rtc, body.sdp) }
                }
            }
            is FfiCallSignalingBody.IceCandidate -> {
                if (mutableState.value == WebCallState.Done) return
                // sdpMLineIndex on the wire is UInt32; libwebrtc
                // takes Int. The valid range for an SDP m-line
                // index is 0..255 in practice, so the cast is
                // loss-free.
                val candidate = RemoteIceCandidate(
                    sdpMid = body.sdpMid,
                    sdpMLineIndex = body.sdpMLineIndex.toInt(),
                    candidateSdp = body.candidate,
                )
                // Before the remote description is set (ringing, or the SDP
                // still being applied) the candidate is kept, not dropped.
                if (remoteCandidates.offer(candidate)) return
                val rtc = this.rtc ?: return
                rtc.addRemoteCandidate(
                    sdpMid = candidate.sdpMid,
                    sdpMLineIndex = candidate.sdpMLineIndex,
                    candidateSdp = candidate.candidateSdp,
                )
            }
            is FfiCallSignalingBody.End -> {
                if (mutableState.value == WebCallState.Done) return
                endRemote(toReason(body.reason))
            }
            // ADR-0083 section 4 — a share state is verified against the
            // conversation's MLS sender before it reaches a call
            // ([applyScreenShare]); the raw body carries no sender and is
            // never applied here.
            is FfiCallSignalingBody.ScreenShare -> Unit
            is FfiCallSignalingBody.GroupStart,
            is FfiCallSignalingBody.GroupJoin,
            is FfiCallSignalingBody.GroupLeave -> {
                // Group-call signaling never belongs to the 1×1
                // [CallSession] (C-11). `ChatRepository`
                // routes these bodies to the group-call ledger
                // (`GroupCallLedger.kt`) and never forwards them
                // to the 1×1 sink, so this arm is unreachable;
                // the exhaustive-when contract just requires we
                // name them, and ignoring them is the safe
                // answer if a future caller gets it wrong.
            }
        }
    }

    // ----------------------------------------------------------------
    // Internal — WebRTC delegate plumbing
    // ----------------------------------------------------------------

    /** Mark the remote description applied and add every candidate that
     *  arrived before it, oldest first. Runs on [scope] right after
     *  `setRemoteDescription` completes. */
    private fun applyBufferedRemoteCandidates(rtc: WebRtcPeerConnection) {
        for (candidate in remoteCandidates.markRemoteDescriptionApplied()) {
            rtc.addRemoteCandidate(
                sdpMid = candidate.sdpMid,
                sdpMLineIndex = candidate.sdpMLineIndex,
                candidateSdp = candidate.candidateSdp,
            )
        }
        val dropped = remoteCandidates.dropped
        if (dropped > 0) {
            android.util.Log.w(
                "CallSession",
                "dropped $dropped remote ICE candidates over the pre-connection buffer capacity",
            )
        }
    }

    /** G3-sframe-native N-6 — arm SFrame encryptors on every
     *  sender of [rtc] using the seed + leaf injected via
     *  [setCallKey]. Returns false on any of:
     *    - [setCallKey] was never called
     *    - `SFrameAvailability.isCallingAvailable` is false
     *    - any sender failed to attach
     *  All three collapse to a refused call (security
     *  invariant #4). */
    private fun attachSFrame(rtc: WebRtcPeerConnection): Boolean {
        val seed = callKeySeed
        val leaf = localLeaf
        if (seed == null || leaf == null) {
            android.util.Log.w(
                "CallSession",
                "attachSFrame: setCallKey was never called — security invariant #4",
            )
            return false
        }
        if (!me.lakko.sframe.SFrameAvailability.isCallingAvailable) {
            android.util.Log.w(
                "CallSession",
                "attachSFrame: SFrameAvailability reports calling unavailable",
            )
            return false
        }
        return rtc.armSFrameEncryptors(seed, leaf)
    }

    /** Wire the wrapper's typed callbacks back into the
     *  session. libwebrtc fires them on internal worker
     *  threads; we hop into [scope] (main dispatcher) before
     *  touching [StateFlow]. */
    private fun installDelegate(rtc: WebRtcPeerConnection) {
        rtc.installDelegate(
            WebRtcPeerConnection.Delegate(
                onLocalCandidate = { sdpMid, sdpMLineIndex, candidateSdp ->
                    // Hop to main dispatcher before composing
                    // — UniFFI is thread-safe but the resulting
                    // bytes must be handed to the caller's
                    // signaling sink on the main thread to
                    // preserve ordering w.r.t. UI state.
                    scope.launch {
                        shipLocalCandidate(sdpMid, sdpMLineIndex, candidateSdp)
                    }
                },
                onConnectionState = { newState ->
                    scope.launch { handleConnectionState(newState) }
                },
                onRemoteVideoTrack = { track, streamId ->
                    // 1×1: a single remote camera track, plus the other
                    // person's shared screen in a stream whose id ends in
                    // `~screen` (ADR-0083 section 4). Hop to the main
                    // dispatcher before touching the StateFlow.
                    scope.launch {
                        if (isScreenStreamId(streamId)) {
                            mutableRemoteScreenTrack.value = track
                        } else {
                            mutableRemoteVideoTrack.value = track
                        }
                    }
                },
                onRemoteReceiver = { receiver, _ ->
                    // The native 1×1 media is SFrame-sealed on the send side
                    // (attachSFrame -> armSFrameEncryptors), so the receive
                    // side must arm a decryptor on EVERY incoming track — the
                    // peer's audio at connect AND the peer's video if they turn
                    // their camera on mid-call (ADR-0032 piece B2b). Without
                    // this the sealed frames never open. Synchronous (not
                    // hopped to [scope]) so the first frames are not dropped
                    // while a dispatch is pending; `callKeySeed` is set via
                    // [setCallKey] before dial/accept, so it is non-null before
                    // any track can arrive. Best-effort like the SFU path — a
                    // failed arm affects media availability, not the fail-closed
                    // send gate.
                    callKeySeed?.let { seed -> rtc.armSFrameDecryptor(receiver, seed) }
                },
            )
        )
    }

    private fun shipLocalCandidate(
        sdpMid: String,
        sdpMLineIndex: Int,
        candidateSdp: String,
    ) {
        try {
            val bytes = composeCallSignaling(
                FfiCallSignalingBody.IceCandidate(
                    callId = callId,
                    candidate = candidateSdp,
                    sdpMid = sdpMid,
                    sdpMLineIndex = sdpMLineIndex.toUInt(),
                ),
            )
            onOutboundSignaling(bytes, wakes.inCall())
        } catch (cause: Throwable) {
            android.util.Log.w("CallSession", "shipLocalCandidate failed", cause)
        }
    }

    /** Map PeerConnection.PeerConnectionState transitions
     *  to the public [WebCallState]. */
    private fun handleConnectionState(newState: PeerConnection.PeerConnectionState) {
        lastConnectionState = newState
        when (newState) {
            PeerConnection.PeerConnectionState.CONNECTED -> {
                if (mutableState.value == WebCallState.Connecting ||
                    mutableState.value == WebCallState.Outgoing
                ) {
                    mutableState.value = WebCallState.Connected
                    wakes.markConnected()
                }
                // A recovery after a blip is what cancels the grace.
                cancelIceGrace()
                // G3-2-conn-quality — begin polling connection
                // stats once the transport is up. Idempotent.
                startStatsPollingIfNeeded()
            }
            PeerConnection.PeerConnectionState.FAILED -> {
                cancelIceGrace()
                endLocal(WebCallEndReason.Error)
            }
            PeerConnection.PeerConnectionState.DISCONNECTED -> {
                // libwebrtc sometimes flaps disconnected → connected on a
                // transient network glitch (wifi → LTE handover), so this is
                // a grace and not an ending: a recovery through CONNECTED
                // cancels it.
                //
                // This used to do nothing at all, on the ground that "the
                // upper-layer watchdog handles permanent stalls". It does
                // not: the host's establish timer matches only Outgoing,
                // Connecting and an accepted Ringing, so a CONNECTED call
                // whose transport had gone was never ended by anything.
                armIceGrace()
            }
            PeerConnection.PeerConnectionState.CLOSED -> {
                cancelIceGrace()
                if (mutableState.value != WebCallState.Done) {
                    endRemote(WebCallEndReason.HungUp)
                }
            }
            PeerConnection.PeerConnectionState.NEW,
            PeerConnection.PeerConnectionState.CONNECTING,
            -> Unit
        }
    }

    // ----------------------------------------------------------------
    // Internal — perfect negotiation (ADR-0032 piece B2b, 1×1 camera-on)
    // ----------------------------------------------------------------

    /**
     * Tell the video sender's SFrame encryptor the codec the negotiation that
     * has just completed gave its sender (ADR-0071), so the encryptor leaves
     * that codec's frame header in the clear. Without it the encryptor of a
     * one-to-one call stays at "no codec" and keeps nothing clear.
     *
     * Called after every completed offer / answer exchange of this call: the
     * first one in either role, and each renegotiation in either role (a
     * camera turned on mid-call gets its media line, and so its codec, with
     * that exchange's answer). The sender armed mid-call before its answer is
     * told again here; a sender that is not there yet is skipped.
     *
     * Best effort and never a reason to end the call: an encryptor told
     * nothing seals the whole frame, which costs the peer its key-frame
     * detection and never the confidentiality of the media.
     */
    private fun syncSenderCodecsAfterNegotiation(connection: WebRtcPeerConnection) {
        try {
            connection.syncSenderCodecs()
        } catch (cause: RuntimeException) {
            android.util.Log.w("CallSession", "could not tell the video encryptor its codec", cause)
        }
    }

    /**
     * Drive a mid-call renegotiation offer (the offerer role of perfect
     * negotiation). Mirror of the web `onNegotiationNeeded`. Gated so it only
     * fires once the initial exchange has landed ([negotiationReady]), never
     * overlaps an in-flight offer ([makingOffer]), and only while the SDP state
     * is `STABLE`. If it is not ready it marks [pendingRenegotiation] and
     * returns — the [applyRenegotiationOffer] / [applyRenegotiationAnswer] tail
     * re-drives it once the call settles (native has no `onnegotiationneeded`
     * auto re-fire). [makingOffer] stays set until the reneg answer arrives.
     */
    private suspend fun driveRenegotiationOffer(video: Boolean = pendingRenegotiationVideo) {
        val connection = rtc ?: return
        if (!negotiationReady ||
            makingOffer ||
            connection.signalingState() != PeerConnection.SignalingState.STABLE
        ) {
            pendingRenegotiation = true
            pendingRenegotiationVideo = video
            return
        }
        pendingRenegotiation = false
        pendingRenegotiationVideo = video
        makingOffer = true
        try {
            val offerSdp = connection.createRenegotiationOffer()
            val bytes = composeCallSignaling(
                FfiCallSignalingBody.Offer(
                    callId = callId,
                    sdp = offerSdp,
                    createdAtMs = System.currentTimeMillis().toULong(),
                    // `video` used to be hard-coded true, which was correct
                    // while the only renegotiation was a camera turning ON.
                    // The camera can now turn OFF the same way, and an offer
                    // claiming video for a transceiver that has just gone
                    // `recvOnly` would be a lie about what is being sent.
                    media = renegotiationMedia(video).let {
                        FfiCallMediaFlags(audio = it.audio, video = it.video)
                    },
                ),
            )
            // A wake here showed the peer's iPhone an incoming call mid-call.
            onOutboundSignaling(bytes, wakes.renegotiationOffer())
            // makingOffer stays true until applyRenegotiationAnswer clears it.
        } catch (cause: Throwable) {
            makingOffer = false
            android.util.Log.w("CallSession", "renegotiation offer failed", cause)
            // A failed renegotiation leaves the call up as-is; the local
            // self-preview is already live.
        }
    }

    /**
     * Apply a mid-call renegotiation offer from the peer (the answerer role of
     * perfect negotiation). Mirror of the web `applyRenegotiationOffer`.
     *
     * Canonical glare guard: the OFFERER (caller) is IMPOLITE, the ANSWERER
     * (callee) is POLITE (`polite = !isOfferer`). An impolite peer DROPS a
     * colliding offer; a polite peer YIELDS — it rolls its own in-flight offer
     * back (native has no implicit rollback) and answers. Only rolls back when
     * the SDP state is not `STABLE` (i.e. it actually holds a local offer);
     * calling rollback in `stable` is a libwebrtc error. If it rolled back its
     * own offer, that offer is re-driven after this answer settles
     * ([pendingRenegotiation]).
     */
    private suspend fun applyRenegotiationOffer(sdp: String) {
        val connection = rtc ?: return
        val polite = !isOfferer
        val stable = connection.signalingState() == PeerConnection.SignalingState.STABLE
        val readyForOffer = !makingOffer && (stable || isSettingRemoteAnswerPending)
        val ignoreOffer = !polite && !readyForOffer
        if (ignoreOffer) return
        try {
            if (!stable) {
                // Polite peer yields: discard our own in-flight offer, then
                // apply the peer's. Re-drive ours once this answer settles.
                connection.rollbackLocalDescription()
                makingOffer = false
                pendingRenegotiation = true
            }
            // createAnswer sets the remote offer, builds + sets the answer.
            val answerSdp = connection.createAnswer(sdp)
            syncSenderCodecsAfterNegotiation(connection)
            val bytes = composeCallSignaling(
                FfiCallSignalingBody.Answer(callId = callId, sdp = answerSdp),
            )
            onOutboundSignaling(bytes, wakes.inCall())
        } catch (cause: Throwable) {
            android.util.Log.w("CallSession", "renegotiation offer apply failed", cause)
            return
        }
        if (pendingRenegotiation) driveRenegotiationOffer()
    }

    /**
     * Apply a mid-call renegotiation answer (to a reneg offer WE sent). Mirror
     * of the web `applyIncomingSignaling` "answer" reneg branch:
     * [isSettingRemoteAnswerPending] brackets `setRemoteDescription` (it feeds
     * the glare `readyForOffer` predicate), then [makingOffer] clears + any
     * queued renegotiation is re-driven.
     */
    private suspend fun applyRenegotiationAnswer(
        connection: WebRtcPeerConnection,
        sdp: String,
    ) {
        try {
            isSettingRemoteAnswerPending = true
            connection.applyRemoteAnswer(sdp)
            isSettingRemoteAnswerPending = false
            syncSenderCodecsAfterNegotiation(connection)
        } catch (cause: Throwable) {
            isSettingRemoteAnswerPending = false
            android.util.Log.w("CallSession", "renegotiation answer apply failed", cause)
        }
        makingOffer = false
        if (pendingRenegotiation) driveRenegotiationOffer()
        // The answer is the anchor for everything that follows: it is the first
        // moment this device's video sender exists at all, and the first moment
        // the peer has a receiver for it. Both the diagnosis and the keyframe
        // watchdog hang off it rather than off the camera button, because on a
        // live call the answer landed LATER than a button-anchored probe fired,
        // so every number that probe printed described a sender that had not
        // been negotiated yet.
        if (mutableCameraEnabled.value) {
            startKeyframeWatchdog()
            diagnoseMidCallVideo()
        }
    }

    /**
     * Make sure the peer gets a key frame to start from, and say on the console
     * whether it worked.
     *
     * ## What the live run showed
     *
     * A receiver joining a stream in progress cannot decode a delta frame; it
     * discards everything until an IDR arrives, which looks from the outside
     * like packets arriving and no picture appearing. The first attempt at this
     * fired the reconfiguration the instant the answer was applied, reported
     * success, and produced nothing: the receiver asked 78 times over four
     * seconds and got no key frame. The likeliest reason is that at that
     * instant there was **no running encoder to reconfigure** — the sender had
     * been negotiated microseconds earlier and had encoded nothing.
     *
     * So this waits for evidence that the encoder is running
     * (`framesEncoded > 0`) before pulling any lever, then pulls one, then
     * checks whether it worked and escalates to a different lever if not. Every
     * decision is [keyframeAction]'s; this only performs it.
     *
     * Bounded at [MAX_KEYFRAME_ATTEMPTS]: a watchdog that never gives up would
     * reconfigure the encoder for the rest of the call, and the console saying
     * "gave up" is a better report than a loop that hides the failure.
     */
    private fun startKeyframeWatchdog() {
        keyframeWatchdog?.cancel()
        keyframeWatchdog = scope.launch {
            var attempts = 0
            repeat(KEYFRAME_WATCHDOG_TICKS) {
                delay(KEYFRAME_WATCHDOG_INTERVAL_MS)
                val connection = rtc ?: return@launch
                if (!mutableCameraEnabled.value) return@launch
                val facts = try {
                    connection.sampleLocalVideoSendFacts()
                } catch (cause: Throwable) {
                    android.util.Log.w("CallSession", "keyframe: watchdog probe failed", cause)
                    return@launch
                }
                when (keyframeAction(facts, attempts)) {
                    KeyframeAction.SATISFIED -> {
                        android.util.Log.i(
                            "CallSession",
                            "keyframe present after $attempts attempt(s): " +
                                "keyFramesEncoded=${facts.keyFramesEncoded} " +
                                "framesEncoded=${facts.framesEncoded} " +
                                "pliCount=${facts.pliCount}",
                        )
                        return@launch
                    }
                    // The encoder has produced nothing yet. This is the branch
                    // whose absence wasted the last live run.
                    KeyframeAction.WAIT -> Unit
                    KeyframeAction.FORCE -> {
                        attempts += 1
                        val ok = runCatching { connection.forceVideoKeyframe() }.getOrDefault(false)
                        android.util.Log.i(
                            "CallSession",
                            "keyframe force (encoder running): requested=$ok " +
                                "framesEncoded=${facts.framesEncoded} pliCount=${facts.pliCount}",
                        )
                    }
                    KeyframeAction.ESCALATE -> {
                        attempts += 1
                        // A different path into the same encoder: change the
                        // resolution it is fed instead of its send parameters.
                        val capture = localCameraCapture
                        if (capture == null) {
                            android.util.Log.w("CallSession", "keyframe: no capture to nudge")
                            return@launch
                        }
                        runCatching { capture.nudgeOutputFormat() }
                        android.util.Log.i(
                            "CallSession",
                            "keyframe escalation (output-format nudge), attempt $attempts: " +
                                "keyFramesEncoded=${facts.keyFramesEncoded} pliCount=${facts.pliCount}",
                        )
                    }
                    KeyframeAction.GIVE_UP -> {
                        // Said plainly, because the peer is showing nothing and
                        // this device is the reason.
                        android.util.Log.w(
                            "CallSession",
                            "keyframe: no lever produced one after $attempts attempts " +
                                "(framesEncoded=${facts.framesEncoded} " +
                                "keyFramesEncoded=${facts.keyFramesEncoded} " +
                                "pliCount=${facts.pliCount} firCount=${facts.firCount}): " +
                                keyframeGiveUpDiagnosis(facts),
                        )
                        return@launch
                    }
                }
            }
        }
    }

    /** The watchdog for the live camera-on, cancelled when a new one starts. */
    private var keyframeWatchdog: kotlinx.coroutines.Job? = null

    private fun endLocal(reason: WebCallEndReason) {
        if (mutableState.value == WebCallState.Done) return
        mutableState.value = WebCallState.Ending
        mutableEndReason.value = reason
        try {
            val bytes = composeCallSignaling(
                FfiCallSignalingBody.End(
                    callId = callId,
                    reason = fromReason(reason),
                ),
            )
            onOutboundSignaling(bytes, wakes.end())
        } catch (cause: Throwable) {
            android.util.Log.w("CallSession", "endLocal compose failed", cause)
        }
        teardownMediaPipeline()
        pendingRemoteOfferSdp = null
        scope.cancel()
        mutableState.value = WebCallState.Done
    }

    private fun endRemote(reason: WebCallEndReason) {
        if (mutableState.value == WebCallState.Done) return
        mutableEndReason.value = reason
        teardownMediaPipeline()
        pendingRemoteOfferSdp = null
        scope.cancel()
        mutableState.value = WebCallState.Done
    }

    /**
     * G3-2-video-ui — common teardown for the WebRTC pod
     * + camera capture. Both [endLocal] and [endRemote]
     * go through here so a hung-up call and a remote-end
     * call release the camera + clear the Compose preview
     * overlay identically.
     */
    private fun teardownMediaPipeline() {
        // G3-2-conn-quality — stop the stats poll loop before
        // dropping the peer connection.
        statsJob?.cancel()
        statsJob = null
        for (line in inboundCounters.total()) android.util.Log.i("CallSession", line)
        rtc?.close()
        rtc = null
        localCameraCapture?.let { capture ->
            // stopCapture is synchronous + thread-blocking
            // for ~50 ms while libwebrtc's camera thread
            // joins. Doing it inline here keeps the order
            // predictable: by the time `Done` is observed
            // by the UI, the camera is fully released.
            capture.stop()
            capture.dispose()
        }
        localCameraCapture = null
        mutableLocalVideoTrack.value = null
        // G3-2-remote-video — drop the remote handle so the
        // gallery tile reverts to the avatar placeholder on
        // both end paths.
        mutableRemoteVideoTrack.value = null
        // ADR-0083 — the share goes with the call.
        mutableRemoteScreenTrack.value = null
        screenShareReceiver?.close()
        screenShareReceiver = null
        mutableScreenShare.value = null
        // G3-2-conn-quality — clear stale stats (RTT + bitrate).
        mutableRttMs.value = null
        mutableConnectionRelayed.value = false
        mutableDownBps.value = null
        mutableUpBps.value = null
        prevStatsBytes = null
        callPathLog = CallPathLog()
        // ADR-0032 piece B2b — reset perfect-negotiation state so a late
        // callback after teardown starts clean.
        makingOffer = false
        isSettingRemoteAnswerPending = false
        negotiationReady = false
        pendingRenegotiation = false
        addingVideo = false
        // Candidates kept for a remote description that will never be set.
        remoteCandidates.reset()
        // ADR-0032 piece B2a — no camera-on in flight once the media pipeline
        // is torn down.
        mutableVideoPublishState.value = VideoPublishState.Off
    }

    /** G3-2-conn-quality — start the connection-stats poll loop
     *  once, on the first Connected transition. Samples the pod's
     *  `RTCStatsReport` every 2 s and republishes [rttMs] /
     *  [connectionRelayed]. Ends when [statsJob] is cancelled
     *  (teardown) or the peer goes away. */
    /** How long inbound media has been silent ([MediaWatchdog]). */
    private var inboundActivity: MediaWatchdog.InboundActivity? = null

    /** Which network path the media takes, logged when it is first known and
     *  when it changes (`CallPath.kt`). */
    private var callPathLog = CallPathLog()

    /** The ICE-DISCONNECTED grace, armed on the transition and cancelled by a
     *  recovery. See [handleConnectionState]. */
    private var iceGraceJob: Job? = null

    /** The last transport state seen, so the grace can ask what it decided on
     *  without the pod having to publish a getter for it. */
    private var lastConnectionState: PeerConnection.PeerConnectionState =
        PeerConnection.PeerConnectionState.NEW

    /**
     * Arm the ICE-DISCONNECTED grace. Idempotent: a second DISCONNECTED while
     * one is already running keeps the first window rather than extending it,
     * so a flapping transport cannot postpone the ending indefinitely.
     */
    private fun armIceGrace() {
        if (iceGraceJob != null) return
        val s = mutableState.value
        if (s != WebCallState.Connected && s != WebCallState.Connecting) return
        iceGraceJob = scope.launch {
            delay(ICE_DISCONNECTED_GRACE_MS)
            iceGraceJob = null
            when (lastConnectionState) {
                PeerConnection.PeerConnectionState.DISCONNECTED,
                PeerConnection.PeerConnectionState.FAILED,
                -> endLocal(WebCallEndReason.Error)
                else -> Unit
            }
        }
    }

    private fun cancelIceGrace() {
        iceGraceJob?.cancel()
        iceGraceJob = null
    }

    /**
     * End a connected call that has received nothing for
     * [MEDIA_SILENCE_TIMEOUT_MS] ([MediaWatchdog]).
     *
     * Only while Connected: a call still being set up has not received
     * anything YET, which is a different thing from having stopped.
     */
    private fun endIfMediaHasGoneSilent(nowMs: Long) {
        if (mutableState.value != WebCallState.Connected) return
        if (!MediaWatchdog.hasGoneSilent(inboundActivity, nowMs, MEDIA_SILENCE_TIMEOUT_MS)) return
        android.util.Log.w(
            "CallSession",
            "nothing received for $MEDIA_SILENCE_TIMEOUT_MS ms; ending the call",
        )
        endLocal(WebCallEndReason.Error)
    }

    private fun startStatsPollingIfNeeded() {
        if (statsJob != null) return
        statsJob = scope.launch {
            while (isActive) {
                val pod = rtc ?: return@launch
                val snap = pod.sampleConnectionStats()
                if (snap != null) {
                    mutableRttMs.value = snap.rttMs
                    mutableConnectionRelayed.value = snap.relayed
                    // iter2 — derive bitrate from the byte delta against
                    // a monotonic clock (robust to poll-loop jitter; the
                    // 2 s delay is only a nominal cadence).
                    val nowNanos = System.nanoTime()
                    val prev = prevStatsBytes
                    if (snap.hasActivePair && prev != null) {
                        val dtSec = (nowNanos - prev.third) / 1_000_000_000.0
                        if (dtSec > 0.1) {
                            val dRx = (snap.bytesReceived - prev.first).coerceAtLeast(0L)
                            val dTx = (snap.bytesSent - prev.second).coerceAtLeast(0L)
                            mutableDownBps.value = Math.round(dRx * 8.0 / dtSec).toInt()
                            mutableUpBps.value = Math.round(dTx * 8.0 / dtSec).toInt()
                        }
                    }
                    prevStatsBytes = if (snap.hasActivePair) {
                        Triple(snap.bytesReceived, snap.bytesSent, nowNanos)
                    } else {
                        null
                    }
                    val nowMs = nowNanos / 1_000_000L
                    inboundActivity =
                        MediaWatchdog.note(inboundActivity, snap.mediaPacketsReceived, nowMs)
                    for (line in inboundCounters.interval(snap.stats)) {
                        android.util.Log.i("CallSession", line)
                    }
                    callPathLog.note(snap.stats)?.let { android.util.Log.i("CallSession", it) }
                    endIfMediaHasGoneSilent(nowMs)
                }
                delay(2_000)
            }
        }
    }

    /**
     * G3-2-video-ui — common helper for the dial + accept
     * paths. Constructs the camera capture pipeline, attaches
     * the resulting track to the peer connection, starts the
     * capturer. Soft-fail on no-camera / start-failure so
     * the call still proceeds audio-only.
     */
    private fun startLocalCameraCapture(rtc: WebRtcPeerConnection) {
        if (!CameraVideoCapture.hasUsableCamera(applicationContext)) {
            android.util.Log.i("CallSession", "no camera available; staying audio-only")
            return
        }
        val source = WebRtcPeerConnection.newVideoSource(applicationContext)
        val eglContext = WebRtcPeerConnection.sharedEglContext()
        if (source == null || eglContext == null) {
            android.util.Log.w(
                "CallSession",
                "newVideoSource / sharedEglContext returned null — staying audio-only",
            )
            return
        }
        val capture = CameraVideoCapture.create(applicationContext, source, eglContext, preferredCamera)
        if (capture == null) {
            android.util.Log.w("CallSession", "CameraVideoCapture.create returned null")
            return
        }
        mutableCameraIsFrontFacing.value = capture.startsFrontFacing
        rtc.addLocalVideoTrack(source)
        this.localCameraCapture = capture
        mutableLocalVideoTrack.value = rtc.currentLocalVideoTrack()
        // G3-2-call-controls — camera is now live; the in-call
        // camera toggle reflects + drives this from here.
        mutableCameraEnabled.value = true
        try {
            capture.start()
        } catch (cause: CameraVideoCapture.CaptureError) {
            android.util.Log.w("CallSession", "camera start failed", cause)
            // On start failure, drop the capture handle +
            // clear the track ref so the Compose overlay
            // hides. The (now silent) video sender keeps
            // the SDP m=video line, which is fine — the
            // receiver sees a black frame.
            capture.dispose()
            this.localCameraCapture = null
            mutableLocalVideoTrack.value = null
            mutableCameraEnabled.value = false
        }
    }

    private fun toReason(r: FfiCallEndReason): WebCallEndReason = when (r) {
        FfiCallEndReason.HUNG_UP -> WebCallEndReason.HungUp
        FfiCallEndReason.BUSY -> WebCallEndReason.Busy
        FfiCallEndReason.DECLINED -> WebCallEndReason.Declined
        FfiCallEndReason.TIMED_OUT -> WebCallEndReason.TimedOut
        FfiCallEndReason.ERROR -> WebCallEndReason.Error
        // A group / link call reason. In a one-to-one call any End ends it for
        // both, so it reads as the hang-up it is.
        FfiCallEndReason.ENDED_FOR_ALL -> WebCallEndReason.HungUp
    }

    private fun fromReason(r: WebCallEndReason): FfiCallEndReason = when (r) {
        WebCallEndReason.HungUp -> FfiCallEndReason.HUNG_UP
        WebCallEndReason.Busy -> FfiCallEndReason.BUSY
        WebCallEndReason.Declined -> FfiCallEndReason.DECLINED
        WebCallEndReason.TimedOut -> FfiCallEndReason.TIMED_OUT
        WebCallEndReason.Error -> FfiCallEndReason.ERROR
    }
}

/** Convenience wrapper. Caller passes raw bytes; returns
 *  the typed body. */
public fun parseCallSignalingBytes(bytes: ByteArray): FfiCallSignalingBody =
    parseCallSignaling(bytes)
