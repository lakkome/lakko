// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import me.lakko.sframe.SFrameAvailability
import org.webrtc.PeerConnection
import org.webrtc.RtpReceiver
import org.webrtc.VideoTrack
import uniffi.lakko_ffi_uniffi.SfuErrorCode
import uniffi.lakko_ffi_uniffi.SfuPeerSnapshot

/**
 * Multi-party SFU-backed call session on Android — the Kotlin twin of
 * the iOS `apps/ios/Lakko/Calls/SfuCallSession.swift` and the web
 * `apps/web/src/calls/sfu-call-session.ts`.
 *
 * Where the 1×1 `CallSession` connects two devices peer-to-peer over
 * DTLS-SRTP, this connects the device to the Rust SFU and runs SFrame
 * end-to-end media encryption on top (the SFU only ever sees SFrame
 * ciphertext — security invariant #2).
 *
 * Composes [SfuSignalingClient] (the `lakko-sfu-v1` WebSocket, typed
 * frames through `lakko_sfu_proto`), the call-admission JWT (from
 * [CallJwtClient]), and [WebRtcPeerConnection] (the libwebrtc pod, with
 * its `armSFrameEncryptors` / `armSFrameDecryptor` hooks that install
 * the native SFrame bridge on each sender / receiver).
 *
 * ## Signaling flow (one `PeerConnection` to the SFU)
 *   * **Publish** (client-initiated, once at connect): capture the mic,
 *     arm SFrame on the sender(s), `createOffer`, send it as
 *     `ClientFrame::SdpOffer`; the SFU replies `SdpAnswer`.
 *   * **Subscribe** (SFU-initiated): a publishing peer triggers an
 *     `SdpOffer` from the SFU; the session answers (`createAnswer` ->
 *     `sendSdpAnswer`), and the new remote track surfaces through the
 *     pod's `onRemoteReceiver` (SFrame decryptor armed) +
 *     `onRemoteVideoTrack` (UI). Server offers go through a serialized
 *     queue so two applies never overlap.
 *
 * The SFU's own ICE candidates ride inside the SDP, so this session
 * only trickles its LOCAL candidates up via `sendIceCandidate` (the
 * wire protocol has no server -> client ICE frame).
 *
 * Threading: the [SfuSignalingClient] callbacks fire on [scope]'s
 * dispatcher; the [WebRtcPeerConnection] delegate fires on libwebrtc's
 * internal threads, so those closures hop back to [scope] before
 * touching session state. Pass a main-confined scope (e.g. a
 * ViewModel scope) so state mutations stay single-threaded.
 *
 * Scope: audio publish on the first pass (video capture into the SFU
 * session + the `CallUiState` / chat wire-up are follow-ups). `connect`
 * refuses before any capture if the SFrame shim is unavailable
 * (invariant #4).
 */
class SfuCallSession(
    private val applicationContext: Context,
    private val config: Config,
    private val scope: CoroutineScope,
    private val callbacks: Callbacks = Callbacks(),
) {
    enum class State { IDLE, CONNECTING, CONNECTED, FAILED, CLOSED }

    /** Per-call configuration. */
    data class Config(
        val sfuBaseUrl: String,
        val jwt: String,
        /** 32-byte MLS exporter seed (`Group.exportSecret("lakko/call/v1", callId, 32)`). */
        val seed: ByteArray,
        /** This device's MLS leaf index — the SFrame KID it stamps. */
        val localLeaf: Int,
        val audio: Boolean,
        val video: Boolean,
    )

    /** Lifecycle + roster callbacks. */
    data class Callbacks(
        val onStateChange: (State) -> Unit = {},
        val onWelcome: (selfPeerId: String, roster: List<SfuPeerSnapshot>) -> Unit = { _, _ -> },
        val onPeerJoined: (peerId: String, accountPseudonym: String) -> Unit = { _, _ -> },
        val onPeerLeft: (peerId: String) -> Unit = {},
        val onRemoteVideoTrack: (track: VideoTrack?, streamId: String?) -> Unit = { _, _ -> },
        val onServerError: (code: SfuErrorCode, detail: String) -> Unit = { _, _ -> },
    )

    class SfuCallException(message: String) : Exception(message)

    var state: State = State.IDLE
        private set

    private var signaling: SfuSignalingClient? = null
    private var pc: WebRtcPeerConnection? = null
    private val offerQueue = ArrayDeque<String>()
    private var drainingOffers = false

    /**
     * Open the SFU connection, publish the local audio (SFrame-sealed),
     * and start subscribing. Throws before any media capture if the
     * SFrame shim is unavailable (invariant #4).
     */
    suspend fun connect() {
        check(state == State.IDLE) { "connect() called twice" }
        if (!SFrameAvailability.isCallingAvailable) {
            throw SfuCallException("SFrame native shim unavailable — refusing call (invariant #4)")
        }
        setState(State.CONNECTING)

        val sig = SfuSignalingClient(
            baseUrl = config.sfuBaseUrl,
            jwt = config.jwt,
            scope = scope,
            callbacks = signalingCallbacks(),
        )
        signaling = sig
        try {
            sig.connect()
        } catch (e: Throwable) {
            fail()
            throw e
        }

        try {
            openPeerConnectionAndPublish()
        } catch (e: Throwable) {
            fail()
            throw e
        }
    }

    /** Mute / unmute the local microphone (local-only track gate). */
    fun setMicrophoneEnabled(on: Boolean) {
        pc?.setMicrophoneEnabled(on)
    }

    /** Tear down. Idempotent. */
    fun hangUp() {
        signaling?.close()
        signaling = null
        pc?.close()
        pc = null
        offerQueue.clear()
        if (state != State.FAILED && state != State.CLOSED) {
            setState(State.CLOSED)
        }
    }

    // ---- Peer connection + publish ----------------------------------

    private suspend fun openPeerConnectionAndPublish() {
        // The pod's ctor defaults `iceServers` to its own
        // `defaultIceServers()` (STUN; TURN arrives via the rendezvous
        // bootstrap config), so we don't pass it explicitly.
        val connection = WebRtcPeerConnection(applicationContext = applicationContext)
        connection.installDelegate(peerDelegate())
        pc = connection

        connection.addLocalAudioTrack(enabled = config.audio)
        // Arm SFrame on the sender BEFORE the offer so the SFU never
        // sees a plaintext frame. A failed arm is fail-closed (#4).
        if (!connection.armSFrameEncryptors(config.seed, config.localLeaf)) {
            throw SfuCallException("SFrame encryptor arm failed — refusing call (invariant #4)")
        }
        val offer = connection.createOffer(audio = config.audio, video = config.video)
        signaling?.sendSdpOffer(offer)
    }

    // ---- Signaling --------------------------------------------------

    private fun signalingCallbacks(): SfuSignalingCallbacks =
        object : SfuSignalingCallbacks {
            override fun onWelcome(
                selfPeerId: String,
                callId: String,
                roster: List<SfuPeerSnapshot>,
            ) {
                callbacks.onWelcome(selfPeerId, roster)
            }

            override fun onPeerJoined(peerId: String, accountPseudonym: String) {
                callbacks.onPeerJoined(peerId, accountPseudonym)
            }

            override fun onPeerLeft(peerId: String) {
                callbacks.onPeerLeft(peerId)
            }

            override fun onSdpAnswer(sdp: String) {
                scope.launch { applyRemoteAnswer(sdp) }
            }

            override fun onSdpOffer(sdp: String) {
                offerQueue.addLast(sdp)
                scope.launch { drainServerOffers() }
            }

            override fun onServerError(code: SfuErrorCode, detail: String) {
                callbacks.onServerError(code, detail)
            }

            override fun onClosed() {
                onSignalingClosed()
            }
        }

    private suspend fun applyRemoteAnswer(sdp: String) {
        val connection = pc ?: return
        try {
            connection.applyRemoteAnswer(sdp)
        } catch (e: Throwable) {
            fail()
        }
    }

    private suspend fun drainServerOffers() {
        if (drainingOffers) return
        drainingOffers = true
        try {
            while (offerQueue.isNotEmpty()) {
                val sdp = offerQueue.removeFirst()
                val connection = pc ?: break
                val sig = signaling ?: break
                try {
                    val answer = connection.createAnswer(sdp)
                    sig.sendSdpAnswer(answer)
                } catch (e: Throwable) {
                    // A single bad renegotiation drops that subscription;
                    // the call stays up. Continue draining.
                }
            }
        } finally {
            drainingOffers = false
        }
    }

    private fun onSignalingClosed() {
        if (state == State.CLOSED || state == State.FAILED) return
        fail()
    }

    // ---- Peer connection delegate -----------------------------------

    private fun peerDelegate(): WebRtcPeerConnection.Delegate =
        WebRtcPeerConnection.Delegate(
            onLocalCandidate = { _, _, candidateSdp ->
                // trySend is thread-safe + order-preserving; no hop needed.
                signaling?.sendIceCandidate(candidateSdp)
            },
            onConnectionState = { rtcState ->
                scope.launch { onConnectionState(rtcState) }
            },
            onRemoteVideoTrack = { track, streamId ->
                scope.launch { callbacks.onRemoteVideoTrack(track, streamId) }
            },
            onRemoteReceiver = { receiver: RtpReceiver, _ ->
                // Arm the SFrame decryptor on every incoming track
                // (audio + video) — invariant #4. Synchronous so the
                // first frames are not dropped while a hop is pending;
                // `pc` is set before any track can arrive.
                pc?.armSFrameDecryptor(receiver, config.seed)
            },
        )

    private fun onConnectionState(rtcState: PeerConnection.PeerConnectionState) {
        when (rtcState) {
            PeerConnection.PeerConnectionState.CONNECTED ->
                if (state == State.CONNECTING) setState(State.CONNECTED)
            PeerConnection.PeerConnectionState.FAILED,
            PeerConnection.PeerConnectionState.DISCONNECTED,
            PeerConnection.PeerConnectionState.CLOSED ->
                fail()
            else -> Unit
        }
    }

    // ---- State ------------------------------------------------------

    private fun setState(next: State) {
        state = next
        callbacks.onStateChange(next)
    }

    private fun fail() {
        if (state == State.FAILED || state == State.CLOSED) return
        setState(State.FAILED)
        hangUp()
    }
}
