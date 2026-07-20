// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import android.util.Log
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.suspendCancellableCoroutine
import org.webrtc.AudioSource
import org.webrtc.AudioTrack
import org.webrtc.DataChannel
import org.webrtc.DefaultVideoDecoderFactory
import org.webrtc.DefaultVideoEncoderFactory
import org.webrtc.EglBase
import org.webrtc.IceCandidate
import org.webrtc.MediaConstraints
import org.webrtc.MediaStream
import org.webrtc.PeerConnection
import org.webrtc.PeerConnectionFactory
import org.webrtc.RtpReceiver
import org.webrtc.SdpObserver
import org.webrtc.SessionDescription
import org.webrtc.VideoSource
import org.webrtc.VideoTrack

/**
 * Lakko's thin wrapper over `org.webrtc.PeerConnection` (G3-2).
 *
 * Mirror of `apps/ios/Lakko/Calls/WebRTCPeerConnection.swift`.
 * Every libwebrtc Java API call lives inside this one file so
 * the rest of the Android app stays platform-neutral —
 * symmetric to the Rust core's posture between `lakko-calls`
 * and `lakko-transport`.
 *
 * ## What it owns
 *
 * - One `PeerConnection` instance per 1×1 call.
 * - The process-wide `PeerConnectionFactory` (libwebrtc's
 *   docs explicitly recommend a single factory per process —
 *   instantiating per-call leaks audio I/O units on Android).
 * - The local audio track (audio-only on first pass; video
 *   plumbing is a clean extension point via
 *   [addLocalVideoTrack]).
 *
 * ## What it does NOT own
 *
 * - The signaling channel — the caller ([CallSession])
 *   handles `composeCallSignaling(...)` and chat-route
 *   delivery. This wrapper only emits **typed callbacks**
 *   when it has something the caller must ship.
 * - The state machine — [CallSession.WebCallState] is the
 *   single source of truth. This wrapper surfaces the raw
 *   [PeerConnection.PeerConnectionState] transitions; the
 *   session translates them into the user-visible state.
 * - The MLS-derived call key — SFrame integration arrives
 *   in a follow-up sub-sprint (`G3-2-sframe`), at which
 *   point the wrapper grows an `installCallKey(...)` hook
 *   wired to libwebrtc's `FrameDecryptor` /
 *   `FrameEncryptor` insertable-streams equivalent. Today
 *   the wrapper is **transport-only** — audio/video bytes
 *   ride plain SRTP without the end-to-end SFrame layer.
 *   Documented as deferred so the audit pack does not
 *   treat the Android call surface as E2EE-complete
 *   prematurely.
 *
 * ## Threading
 *
 * libwebrtc's PeerConnection methods are safe to call from
 * any thread; the observer callbacks fire on libwebrtc's
 * internal network/worker threads. The [Delegate] callbacks
 * we surface to the caller are invoked **on those internal
 * threads** — the caller is responsible for hopping back to
 * the main dispatcher (typically by emitting through a
 * MutableStateFlow which Compose collects on the main
 * thread). This is the same posture
 * `RTCPeerConnectionDelegate` has on iOS.
 */
public class WebRtcPeerConnection(
    private val applicationContext: Context,
    private val iceServers: List<PeerConnection.IceServer> = defaultIceServers(),
) {

    /**
     * Caller-supplied callback surface. The wrapper invokes
     * these closures on libwebrtc's internal threads — the
     * caller MUST hop back to its UI dispatcher before
     * touching any `@Composable` state.
     */
    public data class Delegate(
        /**
         * Emitted when `PeerConnection` produces a local
         * ICE candidate. The caller ships the
         * `(sdpMid, sdpMLineIndex, sdp)` tuple over the
         * call-signaling channel as a
         * `FfiCallSignalingBody.IceCandidate`.
         */
        public val onLocalCandidate: (sdpMid: String, sdpMLineIndex: Int, candidateSdp: String) -> Unit,
        /**
         * Emitted on every connection-state transition.
         * The caller maps `CONNECTED` to
         * `WebCallState.Connected`, `DISCONNECTED` /
         * `FAILED` / `CLOSED` to call teardown.
         */
        public val onConnectionState: (PeerConnection.PeerConnectionState) -> Unit,
        /**
         * G3-2-remote-video — emitted when a remote
         * `VideoTrack` is added (non-null) via libwebrtc's
         * Unified-Plan `onAddTrack`. `streamId` is the MSID
         * of the owning media stream — on a call routed
         * through the Lakko SFU this equals the originating
         * peer id (`negotiate_if_needed` sets the MSID stream
         * group to the source peer id), which a future N-way
         * sprint will use to map the track to a specific
         * participant leaf. On the 1×1 direct-PeerConnection
         * path there is exactly one remote track and the
         * caller renders it for the sole remote participant.
         * Defaulted to a no-op so existing test constructions
         * stay source-compatible.
         */
        public val onRemoteVideoTrack: (track: VideoTrack?, streamId: String?) -> Unit = { _, _ -> },
        /**
         * Emitted for EVERY freshly-added remote `RtpReceiver` (audio
         * AND video), unlike [onRemoteVideoTrack] which only surfaces
         * video to the UI. The SFU session uses this to arm the SFrame
         * decryptor on each incoming track via [armSFrameDecryptor] —
         * invariant #4 requires every remote track be decrypting, not
         * just video. `streamId` is the MSID stream group = the source
         * peer id on an SFU-routed call. Defaulted no-op so the 1×1
         * direct path is unaffected.
         */
        public val onRemoteReceiver: (receiver: RtpReceiver, streamId: String?) -> Unit = { _, _ -> },
    )

    private var delegate: Delegate? = null

    /** The underlying PeerConnection. `null` once [close] runs. */
    private var peer: PeerConnection? = null

    /** Local audio track. Held so [setMicrophoneEnabled] can
     *  toggle it without a renegotiation. */
    private var localAudioTrack: AudioTrack? = null

    /** Backing audio source — needs to outlive the track for
     *  libwebrtc to keep capturing. Owned alongside the track. */
    private var localAudioSource: AudioSource? = null

    /** Local video track. Lazily added by [addLocalVideoTrack]. */
    private var localVideoTrack: VideoTrack? = null

    /** G3-sframe-native N-5 — the `RtpSender` returned by
     *  `peer.addTrack(audioTrack, ...)`. Captured so SFrame
     *  attachment via [armSFrameEncryptors] can call
     *  `sender.setFrameEncryptor(LakkoFrameEncryptor)` once
     *  the per-call MLS exporter seed is known. */
    internal var localAudioSender: org.webrtc.RtpSender? = null

    /** Same shape as [localAudioSender], populated by
     *  [addLocalVideoTrack] on a video-enabled call. */
    internal var localVideoSender: org.webrtc.RtpSender? = null

    /** G3-sframe-native N-5 — Kotlin-side strong references to
     *  the SFrame encryptors attached to local senders. We hold
     *  these so the underlying native pointer stays alive even
     *  after libwebrtc's Java glue temporarily drops its own
     *  reference during sender renegotiation; [armSFrameEncryptors]
     *  releases the prior entries before re-attaching. */
    private val attachedEncryptors:
        MutableList<me.lakko.sframe.LakkoFrameEncryptor> = mutableListOf()

    /** Same shape as [attachedEncryptors] for the decryptor
     *  side. */
    private val attachedDecryptors:
        MutableList<me.lakko.sframe.LakkoFrameDecryptor> = mutableListOf()

    /**
     * Bridge libwebrtc's `PeerConnection.Observer` into the
     * typed [Delegate] surface. Declared before `init` so
     * the `createPeerConnection(config, observer)` call in
     * `init` can reference it without hitting Kotlin's
     * forward-reference compile error.
     *
     * Most callbacks are no-ops for the 1×1 audio-only
     * path; we'll fill them in for group calls + video in
     * subsequent sub-sprints.
     */
    private val internalObserver = object : PeerConnection.Observer {
        override fun onSignalingChange(state: PeerConnection.SignalingState?) {}
        override fun onIceConnectionChange(state: PeerConnection.IceConnectionState?) {}
        override fun onIceConnectionReceivingChange(receiving: Boolean) {}
        override fun onIceGatheringChange(state: PeerConnection.IceGatheringState?) {}
        override fun onIceCandidate(candidate: IceCandidate?) {
            val c = candidate ?: return
            val mid = c.sdpMid ?: return
            delegate?.onLocalCandidate(mid, c.sdpMLineIndex, c.sdp)
        }
        override fun onIceCandidatesRemoved(candidates: Array<out IceCandidate>?) {}
        override fun onAddStream(stream: MediaStream?) {}
        override fun onRemoveStream(stream: MediaStream?) {}
        override fun onDataChannel(channel: DataChannel?) {}
        override fun onRenegotiationNeeded() {}
        override fun onAddTrack(receiver: RtpReceiver?, streams: Array<out MediaStream>?) {
            val r = receiver ?: return
            // MSID stream group id — on an SFU-routed call this
            // is the originating peer id (see Delegate doc).
            val streamId = streams?.firstOrNull()?.id
            // Surface EVERY receiver (audio + video) so the SFU session
            // can arm the SFrame decryptor on each incoming track
            // (invariant #4).
            delegate?.onRemoteReceiver(r, streamId)
            // Video tracks additionally surface to the UI for rendering;
            // the incoming audio track plays out via libwebrtc's audio
            // device automatically and needs no UI handle.
            val track = r.track() as? VideoTrack ?: return
            delegate?.onRemoteVideoTrack(track, streamId)
        }
        override fun onConnectionChange(state: PeerConnection.PeerConnectionState?) {
            val s = state ?: return
            delegate?.onConnectionState(s)
        }
    }

    init {
        ensureFactoryInitialised(applicationContext)
        val config = PeerConnection.RTCConfiguration(iceServers).apply {
            // Force `unified-plan` SDP — has been the libwebrtc
            // default since M89 but pinning explicitly defends
            // against a future bump that flips it back.
            sdpSemantics = PeerConnection.SdpSemantics.UNIFIED_PLAN
            // Bundle every media line on one ICE component —
            // matches what `services/sfu/src/media.rs`
            // negotiates with `Rtc::sdp_api`.
            bundlePolicy = PeerConnection.BundlePolicy.MAXBUNDLE
            rtcpMuxPolicy = PeerConnection.RtcpMuxPolicy.REQUIRE
            // Gather forever; the SFU's `gather_continually`
            // posture mirrors this.
            continualGatheringPolicy =
                PeerConnection.ContinualGatheringPolicy.GATHER_CONTINUALLY
        }
        peer = sharedFactory!!.createPeerConnection(config, internalObserver)
    }

    /**
     * Wire the caller-supplied callbacks. Call exactly once,
     * right after construction. Subsequent calls replace the
     * previous delegate — the delegate is in-process state,
     * not part of the wire protocol.
     */
    public fun installDelegate(delegate: Delegate) {
        this.delegate = delegate
    }

    /** `true` if the underlying PeerConnection was allocated
     *  successfully. */
    public val isReady: Boolean
        get() = peer != null

    /** Idempotent teardown. Releases audio I/O resources, the
     *  PeerConnection itself, fires no further delegate
     *  callbacks. */
    public fun close() {
        try {
            // G3-sframe-native N-5 — release SFrame native
            // pointers BEFORE the senders/receivers / peer
            // are disposed. Otherwise libwebrtc's Java glue
            // releases its scoped_refptr<FrameEncryptorInterface>
            // last, the C++ refcount drops to 0, `delete this`
            // fires, and we're left holding a dangling pointer
            // inside `attachedEncryptors`. Releasing here means
            // our wrapper's Release() runs while libwebrtc still
            // holds its own retain — count goes 2 -> 1, then
            // peer.dispose drops to 0 + frees cleanly.
            attachedEncryptors.forEach { it.release() }
            attachedEncryptors.clear()
            attachedDecryptors.forEach { it.release() }
            attachedDecryptors.clear()

            localAudioTrack?.dispose()
            localAudioSource?.dispose()
            localVideoTrack?.dispose()
            peer?.close()
            peer?.dispose()
        } catch (cause: Throwable) {
            Log.w(TAG, "close: dispose failed", cause)
        }
        peer = null
        localAudioTrack = null
        localAudioSource = null
        localVideoTrack = null
        localAudioSender = null
        localVideoSender = null
    }

    // ----------------------------------------------------------------
    // Local tracks
    // ----------------------------------------------------------------

    /**
     * Attach a local audio track. Audio-only calls call this
     * once during dial / acceptCall; video calls call this +
     * [addLocalVideoTrack].
     */
    public fun addLocalAudioTrack(enabled: Boolean) {
        val factory = sharedFactory ?: return
        val peer = peer ?: return
        val constraints = MediaConstraints()
        val source = factory.createAudioSource(constraints)
        val track = factory.createAudioTrack(AUDIO_TRACK_ID, source)
        track.setEnabled(enabled)
        // `addTrack` returns the RtpSender — we need that for
        // SFrame attachment. The wrapper above already hands the
        // track to libwebrtc; the sender is the right hook for
        // `setFrameEncryptor`.
        val sender = peer.addTrack(track, listOf(LOCAL_STREAM_ID))
        this.localAudioSource = source
        this.localAudioTrack = track
        this.localAudioSender = sender
    }

    /** Toggle the mic. Mute → unmute flips at the encoder
     *  boundary; the remote peer sees silence frames; no
     *  renegotiation. */
    public fun setMicrophoneEnabled(on: Boolean) {
        localAudioTrack?.setEnabled(on)
    }

    /** G3-2-call-controls — toggle the local video track's
     *  enabled flag (the in-call "stop / start camera"
     *  control). Disabling halts frame delivery to the encoder;
     *  no SDP renegotiation. No-op on an audio-only call. */
    public fun setCameraEnabled(on: Boolean) {
        localVideoTrack?.setEnabled(on)
    }

    /** G3-2-conn-quality — one-shot connection-stats sample read
     *  out of WebRTC's `RTCStatsReport`. Surfaces the active
     *  (nominated, succeeded) ICE candidate-pair's round-trip
     *  time + whether the path is relayed (TURN). `rttMs` is
     *  null until WebRTC has an estimate; `hasActivePair` is
     *  false before ICE settles.
     *
     *  G3-2-conn-quality iter2 — `bytesReceived` / `bytesSent` are
     *  the candidate-pair's cumulative wire-byte counters (RTP +
     *  RTCP + STUN on the selected path). A single sample carries
     *  no rate; the caller diffs two samples against its own wall
     *  clock to derive a bitrate. */
    public class ConnectionStatsSnapshot(
        public val rttMs: Int?,
        public val relayed: Boolean,
        public val hasActivePair: Boolean,
        public val bytesReceived: Long,
        public val bytesSent: Long,
    )

    public suspend fun sampleConnectionStats(): ConnectionStatsSnapshot? {
        val p = peer ?: return null
        val report = suspendCancellableCoroutine<org.webrtc.RTCStatsReport?> { cont ->
            try {
                p.getStats { r -> if (cont.isActive) cont.resume(r) }
            } catch (cause: Throwable) {
                if (cont.isActive) cont.resume(null)
            }
        } ?: return null
        val stats = report.statsMap
        val pairs = stats.values.filter { it.type == "candidate-pair" }
        val active = pairs.firstOrNull {
            (it.members["state"] as? String) == "succeeded" &&
                (it.members["nominated"] as? Boolean == true)
        } ?: pairs.firstOrNull { (it.members["state"] as? String) == "succeeded" }
            ?: return ConnectionStatsSnapshot(
                rttMs = null, relayed = false, hasActivePair = false,
                bytesReceived = 0L, bytesSent = 0L,
            )
        val rttSec = (active.members["currentRoundTripTime"] as? Number)?.toDouble()
        val rttMs = rttSec?.let { Math.round(it * 1000.0).toInt() }
        fun candType(id: String?): String? =
            id?.let { stats[it]?.members?.get("candidateType") as? String }
        val relayed = candType(active.members["localCandidateId"] as? String) == "relay" ||
            candType(active.members["remoteCandidateId"] as? String) == "relay"
        val bytesReceived = (active.members["bytesReceived"] as? Number)?.toLong() ?: 0L
        val bytesSent = (active.members["bytesSent"] as? Number)?.toLong() ?: 0L
        return ConnectionStatsSnapshot(
            rttMs = rttMs, relayed = relayed, hasActivePair = true,
            bytesReceived = bytesReceived, bytesSent = bytesSent,
        )
    }

    /**
     * Video plumbing extension point. `addLocalVideoTrack`
     * wires a `VideoTrack` backed by a `VideoSource` the
     * caller supplies — pinning the capture lifecycle to
     * the UI's preview surface is the caller's job.
     *
     * Called from `CallSession.startLocalCameraCapture`
     * (1×1) and `SfuCallSession` (group) on a video-enabled
     * call; audio-only calls skip it.
     *
     * Adding the track fires `onnegotiationneeded` in a
     * generic WebRTC app; the SFU session drives the offer
     * explicitly instead (no `onRenegotiationNeeded` handler),
     * so this is safe to call BOTH at connect time (before the
     * first offer) AND mid-call for the ADR-0032 "camera-on"
     * renegotiation path (`SfuCallSession.publishVideo`).
     */
    public fun addLocalVideoTrack(source: VideoSource) {
        val factory = sharedFactory ?: return
        val peer = peer ?: return
        val track = factory.createVideoTrack(VIDEO_TRACK_ID, source)
        val sender = peer.addTrack(track, listOf(LOCAL_STREAM_ID))
        this.localVideoTrack = track
        this.localVideoSender = sender
    }

    /**
     * G3-2-video-ui — expose the currently-attached local
     * video track so the Compose call screen can hand it
     * to [LocalVideoPreviewView]. Returns `null` if
     * [addLocalVideoTrack] has not run yet (audio-only
     * call, or pre-`startLocalCameraCapture`).
     */
    public fun currentLocalVideoTrack(): VideoTrack? = localVideoTrack

    /**
     * ADR-0032 piece B — `true` when a local video sender with a
     * still-`LIVE` track is already attached (a call started as
     * video, or a prior mid-call `publishVideo`). The SFU session
     * uses this to make `publishVideo` idempotent: rather than
     * acquire a second camera it just re-enables the existing
     * track. A disabled-but-live track (camera toggled off) still
     * reads `true` here, so re-enabling it is a plain
     * `setCameraEnabled(true)`.
     */
    public fun hasLiveLocalVideoSender(): Boolean =
        localVideoSender != null &&
            localVideoTrack?.state() == org.webrtc.MediaStreamTrack.State.LIVE

    /**
     * ADR-0032 piece B — fail-closed removal of the local video
     * sender + track. Called when the mid-call `publishVideo`
     * path added a video sender but then could NOT arm SFrame on
     * it: rather than ship a plaintext video m-line to the SFU
     * (invariant #4), the sender is torn down and the call
     * continues audio-only. No encryptor is released here — this
     * only runs on the arm-FAILED path, where none was attached.
     */
    public fun removeLocalVideoTrack() {
        val peer = peer
        val sender = localVideoSender
        if (peer != null && sender != null) {
            try {
                peer.removeTrack(sender)
            } catch (cause: Throwable) {
                Log.w(TAG, "removeLocalVideoTrack: removeTrack failed", cause)
            }
        }
        localVideoTrack?.dispose()
        localVideoTrack = null
        localVideoSender = null
    }

    // ----------------------------------------------------------------
    // SFrame (G3-sframe-native N-5)
    // ----------------------------------------------------------------

    /**
     * Arm SFrame on every outgoing RtpSender on this peer.
     * Called by `CallSession` once the per-call MLS exporter
     * seed is known (`Group.export_secret("lakko/call/v1",
     * call_id, 32)`).
     *
     * Returns `true` only when every present sender attached
     * successfully. A `false` return is the signal to abort the
     * call — security invariant #4 forbids letting one track
     * flow encrypted while another doesn't.
     */
    public fun armSFrameEncryptors(seed: ByteArray, localLeaf: Int): Boolean {
        if (!me.lakko.sframe.SFrameAvailability.isCallingAvailable) {
            return false
        }
        // Release any prior encryptors first (re-arming on
        // mid-call rotation goes through here).
        attachedEncryptors.forEach { it.release() }
        attachedEncryptors.clear()

        var allOk = true
        // Each sender is bound to its own per-sender stream ordinal (ADR-0035):
        // audio = 0, the (single, v1) video = 1. The seal KID is
        // `(localLeaf << 8) | streamIndex`, so a participant never reuses one
        // (key, nonce) across the two streams. A future second video / screen-
        // share would take stream index 2.
        for ((sender, streamIndex) in listOf(
            localAudioSender to 0,
            localVideoSender to 1,
        )) {
            sender ?: continue
            val enc = me.lakko.sframe.LakkoFrameEncryptor.create(seed, localLeaf, streamIndex)
            if (enc == null) {
                allOk = false
                continue
            }
            sender.setFrameEncryptor(enc)
            attachedEncryptors += enc
        }
        return allOk
    }

    /**
     * ADR-0032 piece B — arm SFrame on ONLY the local **video**
     * sender (the one just added by [addLocalVideoTrack] on the
     * mid-call camera-on path), leaving the already-armed audio
     * encryptor untouched.
     *
     * Unlike [armSFrameEncryptors] (which releases + re-creates
     * every sender's encryptor, and is the connect-time / rekey
     * path), this targets the new video sender only — so the
     * audio encryptor is neither released-without-detach nor
     * needlessly re-derived. The new encryptor is appended to
     * [attachedEncryptors] so the mid-call rekey
     * ([rekeySFrame]) and teardown ([close]) cover it too.
     *
     * Returns `false` on a missing video sender, HKDF failure,
     * or if the SFrame shim is unavailable. A `false` return is
     * the caller's signal to tear the just-added video sender
     * down ([removeLocalVideoTrack]) rather than let it flow
     * plaintext — security invariant #4.
     */
    public fun armSFrameEncryptorForVideoSender(seed: ByteArray, localLeaf: Int): Boolean {
        if (!me.lakko.sframe.SFrameAvailability.isCallingAvailable) {
            return false
        }
        val sender = localVideoSender ?: return false
        // This arm targets the video sender only, so it seals on stream
        // index 1 — the (single, v1) video stream's key space (ADR-0035).
        val enc = me.lakko.sframe.LakkoFrameEncryptor.create(
            seed,
            localLeaf,
            1,
        ) ?: return false
        sender.setFrameEncryptor(enc)
        attachedEncryptors += enc
        return true
    }

    /**
     * Arm SFrame on a freshly-arrived remote `RtpReceiver`.
     * Called by the `onAddTrack` / `onTrack` delegate path when
     * libwebrtc hands us a new incoming track.
     *
     * The decryptor takes NO media/stream argument (ADR-0035): every
     * open reads the composite KID `(sender_leaf << 8) | stream_index`
     * from the frame's own SFrame header and derives that sender+stream's
     * key internally, so one decryptor on a receive track opens audio and
     * every video stream on it. No `receiver.track().kind()` derivation is
     * needed — the per-stream discriminator survives the SFU in the header.
     */
    public fun armSFrameDecryptor(
        receiver: org.webrtc.RtpReceiver,
        seed: ByteArray,
    ): Boolean {
        if (!me.lakko.sframe.SFrameAvailability.isCallingAvailable) {
            return false
        }
        val dec = me.lakko.sframe.LakkoFrameDecryptor.create(seed) ?: return false
        receiver.setFrameDecryptor(dec)
        attachedDecryptors += dec
        return true
    }

    /**
     * Mid-call SFrame rekey (G-18, ADR-0029). Rotate every installed
     * encryptor + decryptor to the new MLS epoch's [seed]. Each encryptor
     * swaps its seal key (it seals only under the current epoch); each
     * decryptor rotates its call-key ring, retaining the previous epoch's
     * key until `nowMs + overlapMs` so in-flight frames sealed under the
     * old epoch still open. Call [pruneSFrameDecryptors] after the window
     * to close it.
     *
     * Returns `true` only when every installed encryptor + decryptor
     * adopted the new key. A `false` return means at least one seal / open
     * path could not — the caller keeps the current seed (never plaintext,
     * security invariant #4). Iterates snapshots so a concurrent
     * [armSFrameDecryptor] on a libwebrtc thread cannot fault the walk.
     */
    public fun rekeySFrame(seed: ByteArray, nowMs: Long, overlapMs: Long): Boolean {
        if (!me.lakko.sframe.SFrameAvailability.isCallingAvailable) {
            return false
        }
        var allOk = true
        for (enc in attachedEncryptors.toList()) {
            if (!enc.rekey(seed)) allOk = false
        }
        for (dec in attachedDecryptors.toList()) {
            if (!dec.rekey(seed, nowMs, overlapMs)) allOk = false
        }
        return allOk
    }

    /**
     * Drop each decryptor's retained previous key once the overlap window
     * has elapsed at [nowMs] (G-18, ADR-0029) — closes the window for
     * seed-boundary forward secrecy (C-9 / C-10). Idempotent.
     */
    public fun pruneSFrameDecryptors(nowMs: Long) {
        for (dec in attachedDecryptors.toList()) {
            dec.prune(nowMs)
        }
    }

    // ----------------------------------------------------------------
    // SDP exchange
    // ----------------------------------------------------------------

    /**
     * Build the SDP offer that goes into
     * `FfiCallSignalingBody.Offer`. Sets the local
     * description as a side-effect.
     */
    public suspend fun createOffer(audio: Boolean, video: Boolean): String {
        val peer = peer ?: throw WebRtcException("PeerConnection is not allocated")
        val constraints = MediaConstraints().apply {
            mandatory.add(
                MediaConstraints.KeyValuePair(
                    "OfferToReceiveAudio",
                    if (audio) "true" else "false",
                )
            )
            mandatory.add(
                MediaConstraints.KeyValuePair(
                    "OfferToReceiveVideo",
                    if (video) "true" else "false",
                )
            )
        }
        val offer = createOfferSuspend(peer, constraints)
        setLocalDescriptionSuspend(peer, offer)
        return offer.description
    }

    /**
     * ADR-0032 piece B — build a RENEGOTIATION offer for a
     * mid-call track change (a camera-on added a video sender
     * after the initial publish). Unlike [createOffer] it uses
     * DEFAULT `MediaConstraints` — no forced
     * `OfferToReceive{Audio,Video}` — so the offer simply
     * reflects the transceivers currently on the peer connection
     * (the audio sender plus the freshly-added video sender).
     * Sets the local description as a side-effect.
     */
    public suspend fun createRenegotiationOffer(): String {
        val peer = peer ?: throw WebRtcException("PeerConnection is not allocated")
        val offer = createOfferSuspend(peer, MediaConstraints())
        setLocalDescriptionSuspend(peer, offer)
        return offer.description
    }

    /** ADR-0032 piece B — the current SDP signaling state, so the
     *  SFU session only fires a renegotiation offer while `STABLE`
     *  (no in-flight offer / answer). `null` once [close] has run. */
    public fun signalingState(): PeerConnection.SignalingState? = peer?.signalingState()

    /**
     * Apply the remote SDP offer + build the answer that goes
     * back over signaling. Sets the local description to the
     * answer; sets the remote description to the offer.
     */
    public suspend fun createAnswer(remoteOfferSdp: String): String {
        val peer = peer ?: throw WebRtcException("PeerConnection is not allocated")
        val remoteOffer = SessionDescription(SessionDescription.Type.OFFER, remoteOfferSdp)
        setRemoteDescriptionSuspend(peer, remoteOffer)
        val answer = createAnswerSuspend(peer, MediaConstraints())
        setLocalDescriptionSuspend(peer, answer)
        return answer.description
    }

    /** Apply the remote SDP answer. Called on the offerer side
     *  after the answerer ships their answer back. */
    public suspend fun applyRemoteAnswer(remoteAnswerSdp: String) {
        val peer = peer ?: throw WebRtcException("PeerConnection is not allocated")
        val remoteAnswer = SessionDescription(SessionDescription.Type.ANSWER, remoteAnswerSdp)
        setRemoteDescriptionSuspend(peer, remoteAnswer)
    }

    /**
     * ADR-0032 piece B2b — explicitly roll back the pending local
     * description (`SessionDescription.Type.ROLLBACK`).
     *
     * The 1×1 P2P renegotiation path is SYMMETRIC, so a glare collision (both
     * peers renegotiating at once) is possible. Canonical perfect-negotiation
     * has the POLITE peer yield: it discards its own in-flight offer, applies
     * the peer's offer, and answers. Unlike the browser — where
     * `setRemoteDescription({type:"offer"})` performs an IMPLICIT rollback —
     * native libwebrtc has no implicit rollback, so the caller must roll back
     * explicitly before applying the colliding remote offer.
     *
     * Only valid while the peer holds a local offer (`have-local-offer`);
     * calling it in the `stable` state is a libwebrtc error, so the caller
     * ([CallSession.applyRenegotiationOffer]) guards on the signaling state.
     * Throws [WebRtcException] on failure.
     *
     * (The SFU session takes a different tack — it SERIALIZES its own offer to
     * completion before applying any SFU offer, so it never needs a rollback.
     * The symmetric 1×1 glare is why this path does.)
     */
    public suspend fun rollbackLocalDescription() {
        val peer = peer ?: throw WebRtcException("PeerConnection is not allocated")
        setLocalDescriptionSuspend(peer, SessionDescription(SessionDescription.Type.ROLLBACK, ""))
    }

    // ----------------------------------------------------------------
    // ICE
    // ----------------------------------------------------------------

    /**
     * Apply a remote-supplied ICE candidate. Best-effort: a
     * failed addIceCandidate is logged but not surfaced —
     * libwebrtc tolerates lost candidates by re-gathering.
     *
     * Note libwebrtc's `addIceCandidate(IceCandidate)` is
     * synchronous (no callback) on the public API used here.
     * The newer overload with an `AddIceObserver` is
     * available but we don't need its richer error signal
     * for the best-effort path.
     */
    public fun addRemoteCandidate(
        sdpMid: String,
        sdpMLineIndex: Int,
        candidateSdp: String,
    ) {
        val peer = peer ?: return
        val candidate = IceCandidate(sdpMid, sdpMLineIndex, candidateSdp)
        peer.addIceCandidate(candidate)
    }

    // ----------------------------------------------------------------
    // SDP callback adapters
    // ----------------------------------------------------------------

    /**
     * Wrap libwebrtc's CompletableFuture-style SdpObserver in
     * a Kotlin suspend function. The split between
     * createOffer/createAnswer and setLocalDescription /
     * setRemoteDescription is libwebrtc's own — each step
     * has its own observer instance.
     */
    private suspend fun createOfferSuspend(
        peer: PeerConnection,
        constraints: MediaConstraints,
    ): SessionDescription = suspendCancellableCoroutine { cont ->
        peer.createOffer(object : SdpObserver {
            override fun onCreateSuccess(sdp: SessionDescription?) {
                if (sdp != null) cont.resume(sdp)
                else cont.resumeWithException(WebRtcException("createOffer returned null SDP"))
            }
            override fun onCreateFailure(reason: String?) {
                cont.resumeWithException(WebRtcException("createOffer failed: ${reason ?: "(unknown)"}"))
            }
            override fun onSetSuccess() {}
            override fun onSetFailure(reason: String?) {}
        }, constraints)
    }

    private suspend fun createAnswerSuspend(
        peer: PeerConnection,
        constraints: MediaConstraints,
    ): SessionDescription = suspendCancellableCoroutine { cont ->
        peer.createAnswer(object : SdpObserver {
            override fun onCreateSuccess(sdp: SessionDescription?) {
                if (sdp != null) cont.resume(sdp)
                else cont.resumeWithException(WebRtcException("createAnswer returned null SDP"))
            }
            override fun onCreateFailure(reason: String?) {
                cont.resumeWithException(WebRtcException("createAnswer failed: ${reason ?: "(unknown)"}"))
            }
            override fun onSetSuccess() {}
            override fun onSetFailure(reason: String?) {}
        }, constraints)
    }

    private suspend fun setLocalDescriptionSuspend(
        peer: PeerConnection,
        sdp: SessionDescription,
    ): Unit = suspendCancellableCoroutine { cont ->
        peer.setLocalDescription(object : SdpObserver {
            override fun onCreateSuccess(sdp: SessionDescription?) {}
            override fun onCreateFailure(reason: String?) {}
            override fun onSetSuccess() {
                cont.resume(Unit)
            }
            override fun onSetFailure(reason: String?) {
                cont.resumeWithException(WebRtcException("setLocalDescription failed: ${reason ?: "(unknown)"}"))
            }
        }, sdp)
    }

    private suspend fun setRemoteDescriptionSuspend(
        peer: PeerConnection,
        sdp: SessionDescription,
    ): Unit = suspendCancellableCoroutine { cont ->
        peer.setRemoteDescription(object : SdpObserver {
            override fun onCreateSuccess(sdp: SessionDescription?) {}
            override fun onCreateFailure(reason: String?) {}
            override fun onSetSuccess() {
                cont.resume(Unit)
            }
            override fun onSetFailure(reason: String?) {
                cont.resumeWithException(WebRtcException("setRemoteDescription failed: ${reason ?: "(unknown)"}"))
            }
        }, sdp)
    }

    public companion object {

        private const val TAG = "WebRtcPeerConnection"
        private const val LOCAL_STREAM_ID = "lakko-local-stream"
        private const val AUDIO_TRACK_ID = "lakko-audio0"
        private const val VIDEO_TRACK_ID = "lakko-video0"

        /**
         * Process-wide singletons. libwebrtc's docs say:
         * > A factory is heavy-weight and should be reused.
         * We lazy-init on the first construction and keep
         * them for the life of the process.
         *
         * `eglBase` is needed even for audio-only calls
         * because the default video encoder/decoder
         * factories construct against an EGL context at
         * factory-init time. The hardware encoder paths
         * (which the audio-only path doesn't touch) call
         * back into it.
         */
        @Volatile private var sharedFactory: PeerConnectionFactory? = null
        @Volatile private var sharedEglBase: EglBase? = null
        private val factoryLock = Any()

        /**
         * Initialise the shared factory if it has not been
         * initialised yet. Thread-safe via double-checked
         * locking; the cost on the second-and-later call is
         * one volatile read.
         */
        private fun ensureFactoryInitialised(context: Context) {
            if (sharedFactory != null) return
            synchronized(factoryLock) {
                if (sharedFactory != null) return
                val app = context.applicationContext
                PeerConnectionFactory.initialize(
                    PeerConnectionFactory.InitializationOptions.builder(app)
                        .setEnableInternalTracer(false)
                        .createInitializationOptions()
                )
                val egl = EglBase.create()
                sharedEglBase = egl
                sharedFactory = PeerConnectionFactory.builder()
                    .setVideoEncoderFactory(
                        DefaultVideoEncoderFactory(egl.eglBaseContext, true, true)
                    )
                    .setVideoDecoderFactory(
                        DefaultVideoDecoderFactory(egl.eglBaseContext)
                    )
                    .createPeerConnectionFactory()
            }
        }

        /**
         * Construct a fresh `VideoSource` bound to the
         * process-wide `PeerConnectionFactory` (G3-2-video).
         * `CameraVideoCapture` (the companion class in this
         * folder) drives frames into the returned source via
         * libwebrtc's `CapturerObserver` plumbing.
         *
         * Returns `null` only if the factory has not been
         * initialised yet — in practice the caller will have
         * already constructed a `WebRtcPeerConnection`, which
         * primes the factory.
         */
        public fun newVideoSource(context: Context): VideoSource? {
            ensureFactoryInitialised(context)
            return sharedFactory?.createVideoSource(false)
        }

        /**
         * Hand the call UI the same EGL context the encoder /
         * decoder factories were built against. The Compose
         * preview surface (`LocalVideoPreviewView`) initialises
         * its `SurfaceViewRenderer` with this exact context so
         * the renderer's GL pipeline shares textures with the
         * codec path — same EGL share-group means zero texture
         * copies between capturer / encoder / preview.
         *
         * Returns `null` if the factory has not been initialised
         * yet; caller should construct a `WebRtcPeerConnection`
         * first (which primes the factory).
         */
        public fun sharedEglContext(): EglBase.Context? =
            sharedEglBase?.eglBaseContext

        /**
         * STUN + (placeholder) TURN config. Production wires
         * these via the rendezvous bootstrap config; for now
         * a single Google STUN server is enough to exercise
         * the happy path on most networks.
         */
        public fun defaultIceServers(): List<PeerConnection.IceServer> = listOf(
            PeerConnection.IceServer.builder(
                listOf(
                    "stun:stun.l.google.com:19302",
                    "stun:stun1.l.google.com:19302",
                )
            ).createIceServer(),
            // TURN servers (Lakko's coturn cluster) arrive via
            // the rendezvous bootstrap config — see
            // `crates/lakko-transport/src/rendezvous.rs`. For
            // now we run STUN-only; on a NAT-symmetric
            // network the call will fail to connect and
            // surface as `DISCONNECTED`. The follow-up
            // sub-sprint G3-2-turn wires the bootstrap config
            // into here.
        )
    }
}

/** Failure mode the wrapper surfaces. */
public class WebRtcException(message: String) : RuntimeException(message)
