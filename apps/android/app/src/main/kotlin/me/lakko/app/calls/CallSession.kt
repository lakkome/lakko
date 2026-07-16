// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import java.security.SecureRandom
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
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
    public val callId: ByteArray,
    private val onOutboundSignaling: (ByteArray) -> Unit,
) {

    /** Lifecycle phases. Mirror of `lakko_calls::CallState`. */
    public enum class WebCallState { Idle, Outgoing, Ringing, Connecting, Connected, Ending, Done }

    /** Reason carried in the terminal transition. */
    public enum class WebCallEndReason { HungUp, Busy, Declined, TimedOut, Error }

    private val mutableState: MutableStateFlow<WebCallState> = MutableStateFlow(WebCallState.Idle)
    public val state: StateFlow<WebCallState> = mutableState.asStateFlow()

    private val mutableEndReason: MutableStateFlow<WebCallEndReason?> = MutableStateFlow(null)
    public val endReason: StateFlow<WebCallEndReason?> = mutableEndReason.asStateFlow()

    private var isOfferer: Boolean = false

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

    /** In-flight guard for [addLocalVideo] (no double camera acquire). */
    private var addingVideo = false

    /**
     * Stored remote SDP offer between `Ringing` →
     * `Connecting`. [acceptCall] reads it to build the
     * answer.
     */
    private var pendingRemoteOfferSdp: String? = null

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
    public fun setCallKey(seed: ByteArray, localLeaf: Int) {
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
     * (fail-soft STUN-only path).
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
    public fun dial(audio: Boolean = true, video: Boolean = false) {
        if (mutableState.value != WebCallState.Idle) return
        isOfferer = true
        mutableState.value = WebCallState.Outgoing

        // G3-2-turn — fetch the TURN credential first so the
        // RTCPeerConnection sees the relay candidate in its
        // initial configuration (same rationale as iOS).
        scope.launch {
            val iceServers = resolveIceServers()
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
                onOutboundSignaling(bytes)
            } catch (cause: Throwable) {
                android.util.Log.w("CallSession", "dial offer-build failed", cause)
                endLocal(WebCallEndReason.Error)
            }
        }
    }

    /**
     * G3-2-turn — build the ICE-server list for this call:
     * `defaultIceServers()` (STUN-only baseline) plus
     * optionally one TURN row freshly minted via
     * [TurnCredentialClient.fetchOrNull]. Fails soft to
     * STUN-only when [turnClient] is unset or the fetch
     * returns null.
     */
    private suspend fun resolveIceServers(): List<PeerConnection.IceServer> {
        val base = WebRtcPeerConnection.defaultIceServers().toMutableList()
        val row = turnClient?.fetchOrNull()
        if (row != null) base.add(row)
        return base
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
            throw WebRtcException(
                "SFrame keying unavailable — refusing mid-call video (invariant #4)",
            )
        }
        addingVideo = true
        try {
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
            val capture = CameraVideoCapture.create(applicationContext, source, eglContext)
                ?: throw WebRtcException("could not create the camera capture")
            try {
                capture.start()
            } catch (cause: CameraVideoCapture.CaptureError) {
                capture.dispose()
                throw WebRtcException("camera start failed: ${cause.message}")
            }

            // 4. Re-read the pc AFTER the (blocking) capture start — a teardown
            //    may have interleaved. If it went away, abandon cleanly.
            val connection2 = rtc
            if (connection2 == null) {
                capture.stop()
                capture.dispose()
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

            // 6. Renegotiate so the peer learns about the new (sealed) video
            //    m-line.
            driveRenegotiationOffer()
        } finally {
            addingVideo = false
        }
    }

    /** Outbound or active call teardown. */
    public fun hangUp(reason: WebCallEndReason = WebCallEndReason.HungUp) {
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
    public fun acceptCall(withVideo: Boolean = false) {
        if (mutableState.value != WebCallState.Ringing || isOfferer) return
        val offerSdp = pendingRemoteOfferSdp
        if (offerSdp == null) {
            android.util.Log.w("CallSession", "acceptCall: pendingRemoteOfferSdp missing — race?")
            endLocal(WebCallEndReason.Error)
            return
        }

        scope.launch {
            val iceServers = resolveIceServers()
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
                endLocal(WebCallEndReason.Error)
                return@launch
            }
            this@CallSession.rtc = rtc

            mutableState.value = WebCallState.Connecting

            try {
                val answerSdp = rtc.createAnswer(remoteOfferSdp = offerSdp)
                val bytes = composeCallSignaling(
                    FfiCallSignalingBody.Answer(
                        callId = callId,
                        sdp = answerSdp,
                    ),
                )
                onOutboundSignaling(bytes)
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

    /** Inbound side: decline. */
    public fun declineCall() {
        endLocal(WebCallEndReason.Declined)
    }

    // ----------------------------------------------------------------
    // Inbound
    // ----------------------------------------------------------------

    /** Route a parsed signaling payload into the state
     *  machine. Caller has already verified call_id matches
     *  this session. */
    public fun applyIncomingSignaling(body: FfiCallSignalingBody) {
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
                            // ADR-0032 piece B2b — arm renegotiation for future
                            // local track changes; drive a camera-on that was
                            // queued before the initial answer landed.
                            negotiationReady = true
                            if (pendingRenegotiation) driveRenegotiationOffer()
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
                val rtc = this.rtc ?: return
                // sdpMLineIndex on the wire is UInt32; libwebrtc
                // takes Int. The valid range for an SDP m-line
                // index is 0..255 in practice, so the cast is
                // loss-free.
                rtc.addRemoteCandidate(
                    sdpMid = body.sdpMid,
                    sdpMLineIndex = body.sdpMLineIndex.toInt(),
                    candidateSdp = body.candidate,
                )
            }
            is FfiCallSignalingBody.End -> {
                if (mutableState.value == WebCallState.Done) return
                endRemote(toReason(body.reason))
            }
            is FfiCallSignalingBody.GroupStart,
            is FfiCallSignalingBody.GroupJoin,
            is FfiCallSignalingBody.GroupLeave -> {
                // Group-call signaling (Phase 3c2) is handled
                // by the in-chat joinable-call card
                // dispatcher, not by the 1×1 [CallSession].
                // The chat-route owner filters these away
                // from the 1×1 sink before they reach this
                // method; the exhaustive-when contract just
                // requires we name them.
            }
        }
    }

    // ----------------------------------------------------------------
    // Internal — WebRTC delegate plumbing
    // ----------------------------------------------------------------

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
                onRemoteVideoTrack = { track, _ ->
                    // 1×1: a single remote video track. The
                    // stream id is ignored here; the N-way SFU
                    // path will key on it to map the track to a
                    // specific participant leaf. Hop to the main
                    // dispatcher before touching the StateFlow.
                    scope.launch { mutableRemoteVideoTrack.value = track }
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
            onOutboundSignaling(bytes)
        } catch (cause: Throwable) {
            android.util.Log.w("CallSession", "shipLocalCandidate failed", cause)
        }
    }

    /** Map PeerConnection.PeerConnectionState transitions
     *  to the public [WebCallState]. */
    private fun handleConnectionState(newState: PeerConnection.PeerConnectionState) {
        when (newState) {
            PeerConnection.PeerConnectionState.CONNECTED -> {
                if (mutableState.value == WebCallState.Connecting ||
                    mutableState.value == WebCallState.Outgoing
                ) {
                    mutableState.value = WebCallState.Connected
                }
                // G3-2-conn-quality — begin polling connection
                // stats once the transport is up. Idempotent.
                startStatsPollingIfNeeded()
            }
            PeerConnection.PeerConnectionState.FAILED -> {
                endLocal(WebCallEndReason.Error)
            }
            PeerConnection.PeerConnectionState.DISCONNECTED -> {
                // libwebrtc sometimes flaps disconnected →
                // connected on a transient network glitch
                // (e.g. wifi → LTE handover). Don't tear
                // down — let the wrapper recover. The
                // upper-layer watchdog handles permanent
                // stalls via [WebCallEndReason.TimedOut].
            }
            PeerConnection.PeerConnectionState.CLOSED -> {
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
     * Drive a mid-call renegotiation offer (the offerer role of perfect
     * negotiation). Mirror of the web `onNegotiationNeeded`. Gated so it only
     * fires once the initial exchange has landed ([negotiationReady]), never
     * overlaps an in-flight offer ([makingOffer]), and only while the SDP state
     * is `STABLE`. If it is not ready it marks [pendingRenegotiation] and
     * returns — the [applyRenegotiationOffer] / [applyRenegotiationAnswer] tail
     * re-drives it once the call settles (native has no `onnegotiationneeded`
     * auto re-fire). [makingOffer] stays set until the reneg answer arrives.
     */
    private suspend fun driveRenegotiationOffer() {
        val connection = rtc ?: return
        if (!negotiationReady ||
            makingOffer ||
            connection.signalingState() != PeerConnection.SignalingState.STABLE
        ) {
            pendingRenegotiation = true
            return
        }
        pendingRenegotiation = false
        makingOffer = true
        try {
            val offerSdp = connection.createRenegotiationOffer()
            val bytes = composeCallSignaling(
                FfiCallSignalingBody.Offer(
                    callId = callId,
                    sdp = offerSdp,
                    createdAtMs = System.currentTimeMillis().toULong(),
                    media = FfiCallMediaFlags(audio = true, video = true),
                ),
            )
            onOutboundSignaling(bytes)
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
            val bytes = composeCallSignaling(
                FfiCallSignalingBody.Answer(callId = callId, sdp = answerSdp),
            )
            onOutboundSignaling(bytes)
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
        } catch (cause: Throwable) {
            isSettingRemoteAnswerPending = false
            android.util.Log.w("CallSession", "renegotiation answer apply failed", cause)
        }
        makingOffer = false
        if (pendingRenegotiation) driveRenegotiationOffer()
    }

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
            onOutboundSignaling(bytes)
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
        // G3-2-conn-quality — clear stale stats (RTT + bitrate).
        mutableRttMs.value = null
        mutableConnectionRelayed.value = false
        mutableDownBps.value = null
        mutableUpBps.value = null
        prevStatsBytes = null
        // ADR-0032 piece B2b — reset perfect-negotiation state so a late
        // callback after teardown starts clean.
        makingOffer = false
        isSettingRemoteAnswerPending = false
        negotiationReady = false
        pendingRenegotiation = false
        addingVideo = false
    }

    /** G3-2-conn-quality — start the connection-stats poll loop
     *  once, on the first Connected transition. Samples the pod's
     *  `RTCStatsReport` every 2 s and republishes [rttMs] /
     *  [connectionRelayed]. Ends when [statsJob] is cancelled
     *  (teardown) or the peer goes away. */
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
        val capture = CameraVideoCapture.create(applicationContext, source, eglContext)
        if (capture == null) {
            android.util.Log.w("CallSession", "CameraVideoCapture.create returned null")
            return
        }
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
