// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import me.lakko.sframe.SFrameAvailability
import org.webrtc.PeerConnection
import org.webrtc.RtpReceiver
import org.webrtc.VideoTrack
import uniffi.lakko_ffi_uniffi.SfuErrorCode
import uniffi.lakko_ffi_uniffi.SfuPeerSnapshot
import uniffi.lakko_ffi_uniffi.sfuDecodePayload

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
 * Media: when [Config.video] is set the session captures the local
 * camera ([CameraVideoCapture]), adds the video track BEFORE arming
 * SFrame + building the offer (so the offer advertises a video m-line
 * and the video sender is SFrame-sealed alongside audio), and surfaces
 * the local track through [Callbacks.onLocalVideoTrack] for the
 * self-preview tile. Audio-only calls skip all of that. `connect`
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
        /**
         * 32-byte MLS exporter seed (`Group.exportSecret("lakko/call/v1",
         * callId, 32)`).
         *
         * Nullable for the ADR-0032 multi-joiner deferred-publish path: a
         * link joiner connects signaling-only ([deferPublish]) and does NOT
         * know the seed until it has joined the MLS group via the committer's
         * Welcome, then calls [publish] with the derived seed. Non-null on the
         * normal in-chat group-call path (media publishes immediately).
         */
        val seed: ByteArray?,
        /** This device's MLS leaf index — the SFrame KID it stamps. On the
         *  deferred-publish path the joiner supplies it to [publish]. */
        val localLeaf: Int,
        val audio: Boolean,
        val video: Boolean,
        /**
         * ADR-0032 — when `true`, [connect] opens the SFU signaling socket
         * (so the peer can relay + receive the KeyPackage / Welcome handshake
         * and appear on the roster) but does NOT capture or publish media. The
         * caller drives media on later via [publish] once it holds a valid
         * seed — the fail-closed "join media only after the Welcome" posture
         * (invariant #4). Defaults to `false` (publish immediately).
         */
        val deferPublish: Boolean = false,
    )

    /** Lifecycle + roster callbacks. */
    data class Callbacks(
        val onStateChange: (State) -> Unit = {},
        val onWelcome: (selfPeerId: String, roster: List<SfuPeerSnapshot>) -> Unit = { _, _ -> },
        val onPeerJoined: (peerId: String, accountPseudonym: String, peerTag: String) -> Unit =
            { _, _, _ -> },
        val onPeerLeft: (peerId: String) -> Unit = {},
        val onRemoteVideoTrack: (track: VideoTrack?, streamId: String?) -> Unit = { _, _ -> },
        /** The LOCAL camera video track became available (`config.video`
         *  publish) or was torn down (`null`). Drives the self-preview
         *  tile. */
        val onLocalVideoTrack: (track: VideoTrack?) -> Unit = {},
        /** ADR-0032 — an opaque peer-to-peer relay payload arrived over the
         *  SFU `signal-relay` frame. `from` is the sending peer's SFU peer id;
         *  `payload` is the decoded bytes (the SFU never inspects them —
         *  invariant #2). The multi-joiner orchestrator classifies these with
         *  `dispatchIncomingEnvelope` (CallKeyPackage / CallWelcome / commit). */
        val onSignalRelay: (from: String, payload: ByteArray) -> Unit = { _, _ -> },
        val onServerError: (code: SfuErrorCode, detail: String) -> Unit = { _, _ -> },
    )

    class SfuCallException(message: String) : Exception(message)

    var state: State = State.IDLE
        private set

    private var signaling: SfuSignalingClient? = null
    private var pc: WebRtcPeerConnection? = null
    /** Local camera capturer — held for the call's lifetime so it
     *  outlives [openPeerConnectionAndPublish] and is torn down in
     *  [hangUp]. `null` for an audio-only call (or when no camera
     *  exists). */
    private var localCameraCapture: CameraVideoCapture? = null
    private val offerQueue = ArrayDeque<String>()
    private var drainingOffers = false

    /**
     * Call-key seed of the CURRENT MLS epoch (G-18, ADR-0029). Starts at
     * [Config.seed] and advances on [rekey], so a late-arriving receiver
     * arms its SFrame decryptor at the current epoch. `@Volatile` because
     * [rekey] writes it on [scope] while [peerDelegate]'s `onRemoteReceiver`
     * reads it on a libwebrtc thread.
     */
    @Volatile
    private var activeSeed: ByteArray? = config.seed

    /**
     * This client's SFrame KID (MLS leaf). From [Config.localLeaf], or set by
     * [publish] on the ADR-0032 deferred-publish path (a link joiner learns
     * its leaf only after joining the MLS group).
     */
    private var publishLeaf: Int = config.localLeaf

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

        // ADR-0032 deferred publish — a link joiner stays signaling-only
        // until it holds a Welcome + a derived seed, then calls [publish].
        if (config.deferPublish) return

        try {
            openPeerConnectionAndPublish()
        } catch (e: Throwable) {
            fail()
            throw e
        }
    }

    /**
     * ADR-0032 — publish local media over the already-open signaling socket,
     * arming SFrame on the supplied (or previously-set) seed + leaf. Used by
     * the deferred-publish path once the client has joined the MLS group and
     * derived the per-call seed — media only ever starts AFTER a valid seed is
     * in hand (invariant #4, no plaintext-media fallback). Idempotent: a second
     * call while a peer connection already exists is a no-op.
     *
     * @throws SfuCallException if no seed is available (never publishes
     *         unencrypted).
     */
    suspend fun publish(seed: ByteArray? = null, localLeaf: Int? = null) {
        if (pc != null) return
        if (seed != null) activeSeed = seed
        if (localLeaf != null) publishLeaf = localLeaf
        if (activeSeed == null) {
            throw SfuCallException(
                "publish() called without a seed — refusing to publish media " +
                    "without SFrame encryption (invariant #4)",
            )
        }
        try {
            openPeerConnectionAndPublish()
        } catch (e: Throwable) {
            fail()
            throw e
        }
    }

    /** ADR-0032 — relay an opaque payload to one peer (the KeyPackage /
     *  Welcome handshake). No-op if signaling is not open. */
    fun sendSignalRelayToPeer(peerId: String, payload: ByteArray) {
        signaling?.sendSignalRelayToPeer(peerId, payload)
    }

    /** ADR-0032 — relay an opaque payload to every other peer in the call (a
     *  broadcast KeyPackage request or an MLS commit). No-op if not open. */
    fun sendSignalRelayToCall(payload: ByteArray) {
        signaling?.sendSignalRelayToCall(payload)
    }

    /** Mute / unmute the local microphone (local-only track gate). */
    fun setMicrophoneEnabled(on: Boolean) {
        pc?.setMicrophoneEnabled(on)
    }

    /**
     * Rotate the call key on a mid-call MLS epoch change (G-18, ADR-0029).
     * The caller re-derives `newSeed = group.exportSecret("lakko/call/v1",
     * callId, 32)` after processing a commit that advanced the epoch and
     * passes it here; every installed SFrame encryptor swaps its seal key
     * and every decryptor retains its previous epoch's key for the overlap
     * window so in-flight frames still open, then the window is pruned.
     * Late-arriving receivers arm at the new epoch ([activeSeed]).
     *
     * No-op if the seed is unchanged (the common per-poll case) or the call
     * is torn down. Fail-closed: a rekey failure keeps the current key —
     * never plaintext (security invariant #4).
     */
    fun rekey(newSeed: ByteArray) {
        if (activeSeed?.contentEquals(newSeed) == true) return
        val connection = pc ?: return
        // Advance the active epoch BEFORE fanning out so a receiver that
        // arrives concurrently arms at the new epoch.
        activeSeed = newSeed
        connection.rekeySFrame(newSeed, System.currentTimeMillis(), OVERLAP_MS)
        // Close the overlap window once it has elapsed (drop the previous
        // key -> full seed-boundary forward secrecy, C-9 / C-10). Mirrors
        // the web worker's `overlapMs + 200` prune schedule. A second rekey
        // inside the window schedules its own prune; the ring only ever
        // retains the most-recent previous key, so an early prune is safe.
        scope.launch {
            delay(OVERLAP_MS + PRUNE_MARGIN_MS)
            pc?.pruneSFrameDecryptors(System.currentTimeMillis())
        }
    }

    /** Tear down. Idempotent. */
    fun hangUp() {
        // Stop + release the camera first (stop blocks until libwebrtc
        // releases the device; dispose frees the capturer + EGL pool).
        localCameraCapture?.let { capture ->
            capture.stop()
            capture.dispose()
        }
        localCameraCapture = null
        callbacks.onLocalVideoTrack(null)
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
        // Fail-closed: never open the media path without an SFrame seed
        // (invariant #4). On the normal path `config.seed` is non-null; on
        // the ADR-0032 deferred path [publish] has already set `activeSeed`.
        val seed = activeSeed
            ?: throw SfuCallException(
                "no SFrame seed — refusing to publish media (invariant #4)",
            )
        // The pod's ctor defaults `iceServers` to its own
        // `defaultIceServers()` (STUN; TURN arrives via the rendezvous
        // bootstrap config), so we don't pass it explicitly.
        val connection = WebRtcPeerConnection(applicationContext = applicationContext)
        connection.installDelegate(peerDelegate())
        pc = connection

        // Publish the mic, and the camera when this is a video call.
        // Both tracks must be added BEFORE arming SFrame + building the
        // offer: armSFrameEncryptors walks the present senders, and the
        // offer's m-lines are fixed at createOffer time (client-initiated
        // add-track post-offer is the deferred renegotiation path). So:
        // add audio -> (add video) -> arm SFrame -> offer, guaranteeing
        // the SFU never sees a plaintext frame on either track (#2/#4).
        connection.addLocalAudioTrack(enabled = config.audio)
        if (config.video) {
            startLocalCameraCapture(connection)
        }
        if (!connection.armSFrameEncryptors(seed, publishLeaf)) {
            throw SfuCallException("SFrame encryptor arm failed — refusing call (invariant #4)")
        }
        val offer = connection.createOffer(audio = config.audio, video = config.video)
        signaling?.sendSdpOffer(offer)
    }

    /**
     * Capture the local camera + add a video track to the peer
     * connection. Mirrors `CallSession.startLocalCameraCapture`: build a
     * fresh `VideoSource` + shared EGL context, wrap them in a
     * [CameraVideoCapture] (front camera, 720p/30), add the backing
     * `VideoTrack` to the PC (so the upcoming offer advertises a video
     * m-line + `armSFrameEncryptors` finds the sender), surface the track
     * for the self-preview tile, and start the capturer. Soft-fails to
     * audio-only (no camera / null source / start failure) — the call
     * proceeds; a silent video sender keeps the m=video line and the
     * receiver sees a black frame.
     */
    private fun startLocalCameraCapture(connection: WebRtcPeerConnection) {
        if (!CameraVideoCapture.hasUsableCamera(applicationContext)) {
            android.util.Log.i("SfuCallSession", "no camera available; publishing audio-only")
            return
        }
        val source = WebRtcPeerConnection.newVideoSource(applicationContext)
        val eglContext = WebRtcPeerConnection.sharedEglContext()
        if (source == null || eglContext == null) {
            android.util.Log.w(
                "SfuCallSession",
                "newVideoSource / sharedEglContext returned null — publishing audio-only",
            )
            return
        }
        val capture = CameraVideoCapture.create(applicationContext, source, eglContext)
        if (capture == null) {
            android.util.Log.w("SfuCallSession", "CameraVideoCapture.create returned null")
            return
        }
        connection.addLocalVideoTrack(source)
        localCameraCapture = capture
        callbacks.onLocalVideoTrack(connection.currentLocalVideoTrack())
        try {
            capture.start()
        } catch (cause: CameraVideoCapture.CaptureError) {
            android.util.Log.w("SfuCallSession", "camera start failed", cause)
            capture.dispose()
            localCameraCapture = null
            callbacks.onLocalVideoTrack(null)
        }
    }

    /** Enable / disable the local camera track (local-only gate — only
     *  meaningful when the call was started with [Config.video], since
     *  the video track is added at connect; toggling without a track is
     *  a no-op). */
    fun setCameraEnabled(on: Boolean) {
        pc?.setCameraEnabled(on)
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

            override fun onPeerJoined(peerId: String, accountPseudonym: String, peerTag: String) {
                callbacks.onPeerJoined(peerId, accountPseudonym, peerTag)
            }

            override fun onPeerLeft(peerId: String) {
                callbacks.onPeerLeft(peerId)
            }

            override fun onSignalRelay(from: String, payloadB64: String) {
                // ADR-0032 — base64url-decode the opaque relay payload and
                // hand the raw bytes to the orchestrator on [scope] (it
                // touches MLS group state). A malformed payload is dropped,
                // not fatal.
                val bytes = try {
                    sfuDecodePayload(payloadB64)
                } catch (e: Throwable) {
                    return
                }
                scope.launch { callbacks.onSignalRelay(from, bytes) }
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
                // `pc` is set before any track can arrive. Arms at the
                // CURRENT epoch (`activeSeed`, G-18/ADR-0029) so a receiver
                // that arrives after a mid-call rekey opens frames sealed
                // under the new epoch. Guarded on a non-null seed — on the
                // ADR-0032 deferred path no remote track arrives before
                // publish sets the seed.
                activeSeed?.let { pc?.armSFrameDecryptor(receiver, it) }
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

    private companion object {
        /** SFrame overlap window in ms — mirrors the web `SFRAME_OVERLAP_MS`
         *  and the Rust core `OVERLAP_DEFAULT_MS` (ADR-0008 / ADR-0029). */
        const val OVERLAP_MS = 1000L

        /** Extra margin before pruning so the window fully elapses on the
         *  media threads (mirrors the web worker's `overlapMs + 200`). */
        const val PRUNE_MARGIN_MS = 200L
    }
}
