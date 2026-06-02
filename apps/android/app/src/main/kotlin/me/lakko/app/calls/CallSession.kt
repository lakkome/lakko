// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import java.security.SecureRandom
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import org.webrtc.PeerConnection
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallMediaFlags
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody
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
 * transitions drive [WebCallState]. Audio-only on first pass
 * — video plumbing extension point lives at
 * [WebRtcPeerConnection.addLocalVideoTrack], wired by a
 * follow-up (`G3-2-video`) once the Compose preview surface
 * is in place.
 *
 * ## SFrame deferred
 *
 * Today's transport rides plain SRTP (per-hop encryption
 * only). The end-to-end SFrame layer keyed off the call's
 * MLS exporter arrives in a follow-up sub-sprint
 * (`G3-2-sframe`); the Android call surface is **not yet
 * E2EE-complete** despite the wire saying so. This caveat
 * is repeated verbatim in `docs/security/calls.md` so the
 * audit pack doesn't double-count.
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
                if (mutableState.value != WebCallState.Idle) return
                isOfferer = false
                pendingRemoteOfferSdp = body.sdp
                mutableState.value = WebCallState.Ringing
            }
            is FfiCallSignalingBody.Answer -> {
                if (mutableState.value != WebCallState.Outgoing) return
                mutableState.value = WebCallState.Connecting
                val rtc = this.rtc ?: return
                scope.launch {
                    try {
                        rtc.applyRemoteAnswer(body.sdp)
                    } catch (cause: Throwable) {
                        android.util.Log.w("CallSession", "applyRemoteAnswer failed", cause)
                        endLocal(WebCallEndReason.Error)
                    }
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
