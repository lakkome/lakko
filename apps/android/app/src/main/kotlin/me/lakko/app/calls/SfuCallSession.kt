// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import me.lakko.sframe.SFrameAvailability
import org.webrtc.PeerConnection
import org.webrtc.RtpReceiver
import org.webrtc.VideoSource
import org.webrtc.VideoTrack
import uniffi.lakko_ffi_uniffi.FfiIncomingEnvelopeKind
import uniffi.lakko_ffi_uniffi.SfuErrorCode
import uniffi.lakko_ffi_uniffi.SfuPeerSnapshot
import uniffi.lakko_ffi_uniffi.SfuVideoGiven
import uniffi.lakko_ffi_uniffi.SfuVideoWant
import uniffi.lakko_ffi_uniffi.TileVideoQuality
import uniffi.lakko_ffi_uniffi.composeMediaState
import uniffi.lakko_ffi_uniffi.dispatchIncomingEnvelope
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
 *
 * ## The camera's three senders, and what is asked of the SFU (ADR-0070)
 *
 * The camera is sent as up to three ordinary senders (`CameraSenders.kt`):
 * the main one, a light copy and a middle copy, each sealed under its own
 * SFrame stream index, and every offer names the copies' media lines. Which
 * of them encode and send is `SenderActivity.kt`: the SFU's `video-needed`
 * frames and the people count. What this device shows of each other person
 * is `VideoWants.kt`: the tiles report, and the whole set goes to the SFU in
 * a `video-wants` frame whenever it changes. What the SFU says it gives back
 * (`video-given`, `VideoGiven.kt`) goes to the tiles.
 */
class SfuCallSession(
    private val applicationContext: Context,
    private val config: Config,
    private val scope: CoroutineScope,
    private val callbacks: Callbacks = Callbacks(),
    /** Wall clock for the signaling keepalive; injectable for tests. */
    private val nowMs: () -> Long = { System.currentTimeMillis() },
    /** The shared `middle_sender_wanted` rule: whether a call of this many
     *  people (this device's own person included) sends the middle copy. */
    private val middleSenderWanted: (people: Int) -> Boolean = { people ->
        uniffi.lakko_ffi_uniffi.middleSenderWanted(people.coerceAtLeast(0).toUInt())
    },
) {
    enum class State { IDLE, CONNECTING, CONNECTED, FAILED, CLOSED }

    /**
     * Lifecycle of a mid-call camera-on (ADR-0032 piece B). A group call
     * started audio-only has no video sender; turning the camera on ACQUIRES
     * the camera and renegotiates (adds a track after the initial offer):
     *   * [OFF]      — no video published (the default; also after a failure).
     *   * [STARTING] — the camera capture + addTrack/renegotiation are in
     *                  flight.
     *   * [LIVE]     — the local video track is captured, SFrame-sealed, and
     *                  added (self-preview renders; the SDP renegotiation runs
     *                  in the background via [driveRenegotiationOffer]).
     *   * [FAILED]   — no camera / capture error / SFrame-arm failure; the
     *                  caller reverts the toggle + surfaces a message. The call
     *                  stays up audio-only (invariant #4 — no plaintext,
     *                  half-added sender is ever left behind).
     */
    enum class VideoPublishState { OFF, STARTING, LIVE, FAILED }

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
         *  publish, or a mid-call [publishVideo]) or was torn down (`null`).
         *  Drives the self-preview tile. */
        val onLocalVideoTrack: (track: VideoTrack?) -> Unit = {},
        /** ADR-0032 piece B — mid-call camera-on lifecycle. Drives the camera
         *  button's spinner + the fail-safe revert in the UI. */
        val onVideoPublishState: (state: VideoPublishState) -> Unit = {},
        /** Whether the camera being captured faces the user, when a capture
         *  opens and when [selectCamera] changes it. Decides whether this
         *  device's own picture is mirrored. */
        val onCameraFacing: (isFrontFacing: Boolean) -> Unit = {},
        /** ADR-0032 — an opaque peer-to-peer relay payload arrived over the
         *  SFU `signal-relay` frame. `from` is the sending peer's SFU peer id;
         *  `payload` is the decoded bytes (the SFU never inspects them —
         *  invariant #2). The multi-joiner orchestrator classifies these with
         *  `dispatchIncomingEnvelope` (CallKeyPackage / CallWelcome / commit). */
        val onSignalRelay: (from: String, payload: ByteArray) -> Unit = { _, _ -> },
        /** camera-off-cleanup — a remote peer broadcast its camera on/off
         *  state over the opaque SFU `signal-relay`. `peerId` is the sending
         *  peer's SFU peer id (== the roster tile key); `cameraOn` is the
         *  decoded flag. The adapter maps it to that peer's roster `camOn`,
         *  fixing the mute case (a `videoTrack.setEnabled(false)` fires no
         *  track event, so its tile would otherwise freeze on the last frame). */
        val onRemoteMediaState: (peerId: String, cameraOn: Boolean) -> Unit = { _, _ -> },
        /** What the SFU says it gives this device of each person's video
         *  changed (ADR-0070 step 6, `VideoGiven.kt`): the whole set as it
         *  stands, by SFU peer id. A person it does not name is given what is
         *  asked for. Empty with downlink control off, and at teardown. */
        val onVideoGiven: (given: Map<String, TileVideoQuality>) -> Unit = {},
        val onServerError: (code: SfuErrorCode, detail: String) -> Unit = { _, _ -> },
    )

    class SfuCallException(message: String) : Exception(message)

    var state: State = State.IDLE
        private set

    /** Whether the SFU refused this device because the call had already
     *  ended (`ErrorCode::CallEnded`). Read by the host after a failed
     *  `connect()` to tell that from a connection failure. */
    var endedByServer: Boolean = false
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

    // ---- Signaling keepalive (SfuKeepalive.kt) -------------------------
    // Runs from the moment signaling is open until hangUp. All four fields are
    // read and written on [scope] only (the Pong / Welcome callbacks hop).
    private var keepaliveJob: Job? = null
    /** Last time the SFU proved it is there (the Welcome frame or a Pong). */
    private var lastInboundMs = 0L

    /** The last time the SFU proved it was there (the Welcome or a Pong), on
     *  this session's clock; 0 before the socket opened. What the call record
     *  ends at when the link died before the device noticed ([callEndedAtMs]). */
    public fun lastInboundAtMs(): Long = lastInboundMs
    /** Last time we sent a Ping. */
    private var lastPingMs = 0L
    private var pingNonce = 0UL

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
     * This device's current camera on/off state (camera-off-cleanup),
     * broadcast to peers over the opaque SFU `signal-relay` so their roster
     * tile for us renders correctly even on a mute (which fires no track
     * event). Seeded from [Config.video]; advanced by [setCameraEnabled] +
     * the [publishVideo] LIVE transitions. `@Volatile` because
     * [setCameraEnabled] may be called from a UI thread while the broadcast
     * helpers read it on [scope].
     */
    @Volatile
    private var localCameraOn: Boolean = config.video

    // ---- Perfect-negotiation state (piece B: mid-call add-track) ----------
    // The client is the POLITE peer relative to the SFU, but instead of the
    // web's implicit-rollback glare handling we SERIALIZE (native-simpler):
    // our publishVideo offer completes fully (offer -> answer) BEFORE any SFU
    // subscribe-offer is applied. A colliding SFU offer just waits in the
    // [offerQueue] until our negotiation clears [makingOffer]. All three flags
    // are read/written only on [scope] (single-threaded), so no synchronisation
    // is needed.
    //
    //   * [makingOffer]          — true from the moment we start building our
    //                              own offer (initial publish OR a mid-call
    //                              renegotiation) until its answer arrives.
    //                              [drainServerOffers] leaves SFU offers queued
    //                              while it is set.
    //   * [negotiationReady]     — false during the manual initial publish; the
    //                              first [applyRemoteAnswer] flips it true, from
    //                              when on a mid-call renegotiation may fire.
    //   * [pendingRenegotiation] — a camera-on arrived while we were mid-offer /
    //                              mid-drain; the answer / drain tail re-drives
    //                              it so it is never lost.
    private var makingOffer = false
    private var negotiationReady = false
    private var pendingRenegotiation = false

    /** Mid-call camera-on lifecycle (piece B). */
    private var videoPublishState: VideoPublishState = VideoPublishState.OFF

    // ---- The camera's senders and what is asked of the SFU (ADR-0070) ------
    // Everything below is read and written on [scope] only (the signaling
    // callbacks hop; the tiles report from the main thread [scope] is
    // confined to).

    /** The video source the camera's senders are fed from, while there is a
     *  capture: what a copy of the camera is made over. */
    private var localCameraSource: VideoSource? = null

    /** This device's SFU peer id, from the Welcome. */
    private var selfPeerId: String? = null

    /**
     * The SFU's last `video-needed` frame (`SenderActivity.kt`): which of this
     * device's camera senders somebody is being sent. `null` before the first
     * frame, which means every sender is needed.
     */
    private var videoNeeded: VideoNeeded? = null

    /** Keeps each camera sender's one encoding active or not, as
     *  [sendersActiveNow] says. */
    private val senderActivity: Map<CameraSenderKind, SenderActivity> =
        CAMERA_SENDER_KINDS.associateWith { kind ->
            SenderActivity(kind.wireName) { message -> android.util.Log.w(MEDIA_LOG_TAG, message) }
        }

    /** Timers on [scope], for the coalesced `video-wants` frames. */
    private val timers = object : CallTimers {
        override fun nowMs(): Long = this@SfuCallSession.nowMs()

        override fun schedule(ms: Long, run: () -> Unit): Any = scope.launch {
            delay(ms)
            run()
        }

        override fun cancel(handle: Any) {
            (handle as? Job)?.cancel()
        }
    }

    /**
     * What this device shows of each person (`VideoWants.kt`): the people of
     * the call as the SFU names them, and the picture every drawn tile
     * reports. Sent to the SFU as a whole whenever it changes.
     */
    private val wantsBook = VideoWantsBook()
    private val wantsSender = VideoWantsSender(wantsBook, { frame -> sendVideoWants(frame) }, timers)

    /**
     * What the SFU says it gives of each person (`VideoGiven.kt`). Read by the
     * tiles and the media counters; never fed back into [wantsBook], so the
     * SFU keeps being asked for what the tiles really want.
     */
    private val givenBook = VideoGivenBook()

    /** The publish offer is out: the SFU has media state for this peer, and
     *  takes a `video-wants` frame only from then on. */
    private var publishOfferSent = false

    /**
     * A tile says which picture of [peerId] it shows (`VideoWants.kt`), or
     * `null` when it stops showing one. [tile] is any object that stays the
     * same for the tile's lifetime. The session asks the SFU for the highest
     * quality any tile of a person wants, `light` for a person no tile shows,
     * and tells it whenever that changes. Call on the thread [scope] is
     * confined to.
     */
    fun reportTileVideoQuality(tile: Any, peerId: String, quality: TileVideoQuality?) {
        if (wantsBook.setTile(tile, peerId, quality)) wantsSender.changed()
    }

    /**
     * A `video-given` frame: the SFU's whole set replaces the last one. The
     * tiles are told, and one line says whose shown picture it changed. What
     * this device asks for is left as it is.
     */
    private fun onVideoGivenFrame(entries: List<SfuVideoGiven>) {
        val before = givenBook.snapshot()
        givenBook.replace(entries.map { it.peer to it.quality })
        describeGivenChanges(
            "call video given",
            videoGivenChanges(wantsBook.wants(), { before[it] }, { givenBook.givenOf(it) }),
        )?.let { android.util.Log.i(MEDIA_LOG_TAG, it) }
        callbacks.onVideoGiven(givenBook.snapshot())
    }

    /** Put one `video-wants` frame on the wire; `false` when it cannot go yet
     *  (the sender then tries again at its next call). */
    private fun sendVideoWants(frame: List<Pair<String, TileVideoQuality>>): Boolean {
        val sig = signaling ?: return false
        if (!publishOfferSent) return false
        val sent = sig.sendVideoWants(frame.map { (peer, quality) -> SfuVideoWant(peer, quality) })
        if (sent) android.util.Log.i(MEDIA_LOG_TAG, describeVideoWants(frame))
        return sent
    }

    /**
     * Whether the middle sender sends now: the shared rule
     * (`middle_sender_wanted`) over the people in the call, this device's own
     * person included. `false` while the rule cannot be asked.
     */
    private fun middleWanted(): Boolean =
        try {
            middleSenderWanted(wantsBook.people())
        } catch (cause: RuntimeException) {
            false
        } catch (cause: LinkageError) {
            false
        }

    /**
     * The "camera is on" condition of the sender-activity decision: this
     * device has a live camera track.
     *
     * Deliberately NOT the track's enabled flag. The person turning the
     * camera off disables the camera track and its copies, exactly as before
     * this rule existed: the senders go on sending (black frames, at very
     * little bitrate) and a viewer who joins meanwhile still gets its key
     * frame. The SFU's own bookkeeping for a silent sender covers only a
     * sender it told `false`; a sender this client silenced by itself would
     * be an unannounced stop on the SFU's side. So the person's camera toggle
     * and the SFU's word compose without touching each other: the toggle
     * changes the tracks, the SFU's word changes the encodings.
     */
    private fun cameraIsOn(): Boolean = pc?.hasLiveLocalVideoSender() == true

    /**
     * Which camera senders encode and send now (`SenderActivity.kt`): the
     * camera is on, the sender exists, the people count allows the middle
     * one, and the SFU's last word for the sender is not `false`.
     */
    private fun sendersActiveNow(): Map<CameraSenderKind, Boolean> {
        val connection = pc
        return sendersActive(
            cameraOn = cameraIsOn(),
            middleWanted = middleWanted(),
            needed = videoNeeded,
            exists = { kind -> connection?.hasCameraSender(kind) == true },
        )
    }

    /**
     * Make every camera sender's encoding active or not, as
     * [sendersActiveNow] says now. `setParameters` only: the media lines, the
     * SSRCs and the SFrame encryptors stay and nothing is renegotiated.
     *
     * Called after everything that can change the answer or reset what
     * libwebrtc holds: a `video-needed` frame, a change of the roster, the
     * camera turned off or on, a sender added, a track replaced, a
     * renegotiation, and every counters tick (which is also when a refused
     * switch-on is asked again). A no-op for a sender whose encoding already
     * says so.
     */
    private fun syncSenderActivity() {
        val connection = pc
        val active = sendersActiveNow()
        for (kind in CAMERA_SENDER_KINDS) {
            val activity = senderActivity.getValue(kind)
            activity.bind(connection?.activitySender(kind))
            if (activity.bound) activity.want(active[kind] == true)
        }
    }

    /**
     * A `video-needed` frame: the SFU's whole word replaces the last one, the
     * senders follow at once, and one line says so when it changed which of
     * them is active.
     */
    private fun onVideoNeededFrame(needed: VideoNeeded) {
        val before = sendersActiveNow()
        videoNeeded = needed
        describeVideoNeededChange(needed, before, sendersActiveNow())
            ?.let { android.util.Log.i(MEDIA_LOG_TAG, it) }
        syncSenderActivity()
    }

    /** What the `send video` line says of one sender: the SFU's last word and
     *  whether its encoding is asked to be active. */
    private fun senderActivityNote(kind: CameraSenderKind): SenderActivityNote =
        SenderActivityNote(needed = neededWord(kind, videoNeeded), active = sendersActiveNow()[kind] == true)

    /**
     * Add every copy of the camera this connection does not have yet
     * (ADR-0070 steps 1 and 5), each sealed under its own SFrame stream index
     * in the same step it is added, before any offer can be built (security
     * invariant #4). The light one is created sending; the middle one sending
     * or not as the roster and the SFU say now.
     *
     * Never fails the call: a copy libwebrtc will not take is absent, logged
     * by the connection. Returns whether a copy was added, which a caller
     * mid-call takes as "renegotiate".
     */
    private fun addCameraCopies(connection: WebRtcPeerConnection, source: VideoSource, seed: ByteArray): Boolean {
        // What the people count says of each sender (`cameraSenderPlan`): the
        // light copy is created sending, the middle one only from the count
        // the shared rule names.
        val plan = cameraSenderPlan(wantsBook.people(), middleSenderWanted)
        var added = false
        for (kind in CAMERA_COPY_KINDS) {
            if (connection.hasCameraSender(kind.sender)) continue
            val planned = plan.first { it.kind == kind.sender }
            val active = if (kind == CameraCopyKind.MIDDLE) {
                // And the SFU's last word for it, if it has said one.
                senderActive(
                    CameraSenderKind.MIDDLE,
                    SenderActivityInput(
                        cameraOn = cameraIsOn(),
                        exists = true,
                        middleWanted = planned.active,
                        needed = videoNeeded,
                    ),
                )
            } else {
                null
            }
            if (connection.addCameraCopySender(kind, source, seed, publishLeaf, active)) added = true
        }
        if (added) {
            android.util.Log.i(
                MEDIA_LOG_TAG,
                "call camera senders people=${wantsBook.people()} " +
                    plan.joinToString(" ") { planned ->
                        val has = connection.hasCameraSender(planned.kind)
                        "${planned.kind.wireName}=${if (!has) "none" else if (planned.active) "on" else "off"}"
                    },
            )
        }
        return added
    }

    /** Send an offer of this client's, naming the media lines of the
     *  camera's copies (they have their `mid`s once the local description is
     *  set, which building the offer did). */
    private fun sendOffer(connection: WebRtcPeerConnection, sdp: String) {
        signaling?.sendSdpOffer(
            sdp,
            connection.cameraCopyMids(CameraCopyKind.LIGHT),
            connection.cameraCopyMids(CameraCopyKind.MIDDLE),
        )
    }

    /**
     * The people of the call changed: the middle sender starts or stops with
     * the count, and the SFU is told what this device shows of the people now
     * in it (again, whatever it is, after a join).
     */
    private fun onRosterChanged(joined: Boolean) {
        syncSenderActivity()
        if (joined) wantsSender.resend() else wantsSender.changed()
    }

    private fun setVideoPublishState(next: VideoPublishState) {
        if (videoPublishState == next) return
        videoPublishState = next
        callbacks.onVideoPublishState(next)
    }

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

        // Signaling is open: start the keepalive on BOTH paths (a deferred
        // publisher sits signaling-only for a while and must notice a dead
        // link too). The connect itself counts as the first inbound proof.
        val opened = nowMs()
        lastInboundMs = opened
        lastPingMs = opened
        keepaliveJob = scope.launch {
            while (isActive) {
                delay(SFU_KEEPALIVE_TICK_MS)
                onKeepaliveTick()
            }
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
     * The sound level of this device's microphone and of every incoming
     * stream ([WebRtcPeerConnection.sampleAudioLevels]), or `null` while no
     * media connection is up. Read on this device from decoded audio; nothing
     * is sent.
     */
    suspend fun sampleAudioLevels(): CallAudioLevels? = pc?.sampleAudioLevels()

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
        localCameraSource = null
        callbacks.onLocalVideoTrack(null)
        keepaliveJob?.cancel()
        keepaliveJob = null
        stopMediaCounters()
        // What was asked of the SFU and what it said go with the call.
        wantsSender.reset()
        wantsBook.clear()
        if (givenBook.size() > 0) {
            givenBook.clear()
            callbacks.onVideoGiven(givenBook.snapshot())
        }
        videoNeeded = null
        senderActivity.values.forEach { it.reset() }
        publishOfferSent = false
        signaling?.close()
        signaling = null
        pc?.close()
        pc = null
        offerQueue.clear()
        // Reset perfect-negotiation + video-publish state so a late callback
        // after teardown starts clean (piece B).
        makingOffer = false
        negotiationReady = false
        pendingRenegotiation = false
        setVideoPublishState(VideoPublishState.OFF)
        if (state != State.FAILED && state != State.CLOSED) {
            setState(State.CLOSED)
        }
    }

    // ---- Peer connection + publish ----------------------------------

    /**
     * TURN-credential client, injected by the call-UI layer; `null` means
     * STUN-only ICE.
     *
     * The SFU has a public address, so most clients reach it on a
     * server-reflexive candidate - but one behind a symmetric NAT has none,
     * and for it the relay is the only way into the room.
     */
    private var turnClient: TurnCredentialClient? = null

    public fun setTurnClient(client: TurnCredentialClient?) {
        this.turnClient = client
    }

    /**
     * STUN always, plus TURN when one can be minted. Fail-soft: a client that
     * answers `null` leaves the call on STUN rather than refusing it.
     */
    private suspend fun resolveIceServers(): List<PeerConnection.IceServer> {
        val base = WebRtcPeerConnection.defaultIceServers()
        val row = turnClient?.fetchOrNull() ?: return base
        return base + row
    }

    private suspend fun openPeerConnectionAndPublish() {
        // Fail-closed: never open the media path without an SFrame seed
        // (invariant #4). On the normal path `config.seed` is non-null; on
        // the ADR-0032 deferred path [publish] has already set `activeSeed`.
        val seed = activeSeed
            ?: throw SfuCallException(
                "no SFrame seed — refusing to publish media (invariant #4)",
            )
        val connection = WebRtcPeerConnection(
            applicationContext = applicationContext,
            iceServers = resolveIceServers(),
        )
        connection.installDelegate(peerDelegate())
        pc = connection
        startMediaCounters(connection)

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
        // The camera's copies (ADR-0070 steps 1 and 5), each added and sealed
        // in one step, like every sender before the offer is built.
        localCameraSource?.let { source -> addCameraCopies(connection, source, seed) }
        // Serialize (piece B): mark our initial publish offer in flight so an
        // SFU subscribe-offer arriving during it waits in the queue until the
        // first answer clears `makingOffer` + arms renegotiation.
        makingOffer = true
        val offer = connection.createOffer(audio = config.audio, video = config.video)
        sendOffer(connection, offer)
        // The SFU has media state for this peer from this offer on: tell it
        // what this device shows of everyone.
        publishOfferSent = true
        wantsSender.resend()
        syncSenderActivity()
        // camera-off-cleanup — announce our initial camera state to peers
        // already in the call so their tile for us starts correct (a
        // camera-off join has no track event to imply "off"). Runs on both
        // the normal connect path and the ADR-0032 deferred [publish] path,
        // since both funnel through here once media is actually published.
        broadcastMediaState()
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
        val capture = CameraVideoCapture.create(applicationContext, source, eglContext, preferredCamera)
        if (capture == null) {
            android.util.Log.w("SfuCallSession", "CameraVideoCapture.create returned null")
            return
        }
        callbacks.onCameraFacing(capture.startsFrontFacing)
        // ADR-0067 step 2: the main camera sender, in three temporal layers
        // on a transceiver of its own (one plain encoding when libwebrtc
        // refuses), armed by the caller before any offer exists. The camera's
        // light and middle copies follow once it is sealed ([addCameraCopies]).
        connection.addLocalVideoTrack(source, groupCamera = true)
        localCameraCapture = capture
        localCameraSource = source
        callbacks.onLocalVideoTrack(connection.currentLocalVideoTrack())
        try {
            capture.start()
        } catch (cause: CameraVideoCapture.CaptureError) {
            android.util.Log.w("SfuCallSession", "camera start failed", cause)
            capture.dispose()
            localCameraCapture = null
            // No capture, no copies: the main sender keeps the media line.
            localCameraSource = null
            callbacks.onLocalVideoTrack(null)
        }
    }

    /** Enable / disable the local camera track (local-only gate — a no-op
     *  when no video sender exists yet, e.g. an audio-only call before a
     *  mid-call [publishVideo]). Broadcasts the resulting camera state so
     *  peers flip our roster `camOn` (camera-off-cleanup): a mute
     *  (`setEnabled(false)`) fires no track event, so the signal is the
     *  only way a receiver learns we went dark. */
    fun setCameraEnabled(on: Boolean) {
        // The camera track and the tracks of its copies.
        pc?.setCameraEnabled(on)
        localCameraOn = on
        broadcastMediaState()
        // The encodings are the SFU's and the roster's to switch, not this
        // toggle's ([cameraIsOn]); asked again all the same, in case the
        // platform reset a flag.
        syncSenderActivity()
    }

    /**
     * `docs/ux/calls.md` section 4.1 — flip to the other camera from the button
     * in the self-view's corner. The twin of [CallSession.switchCamera]: the
     * track and sender are untouched, so neither the SFU subscription nor the
     * SFrame encryptor armed on that sender is disturbed, and no peer
     * renegotiates.
     */
    fun switchCamera(onFacingChanged: (isFrontFacing: Boolean) -> Unit = {}) {
        val capture = localCameraCapture ?: return
        capture.switchCamera { switched, isFrontFacing ->
            if (switched) onFacingChanged(isFrontFacing)
        }
    }

    /** The camera chosen in the call's Settings sheet, or `null` for the
     *  default. Held for the call, so a camera started later opens on it. */
    @Volatile
    private var preferredCamera: String? = null

    /**
     * Use the camera named [cameraName] for this call, or the default one for
     * `null` (`docs/ux/calls.md` section 4.1, the Settings sheet).
     *
     * Applies at once when the camera is being captured, by the same swap as
     * [switchCamera] (same track, same sender, same SFrame encryptor, no
     * renegotiation), and reports the new facing through
     * [Callbacks.onCameraFacing]. Otherwise it is kept for when the camera is
     * turned on.
     */
    fun selectCamera(cameraName: String?) {
        preferredCamera = cameraName
        val capture = localCameraCapture ?: return
        val target = cameraName ?: CameraVideoCapture.defaultCameraName(applicationContext) ?: return
        capture.switchTo(target) { switched, isFrontFacing ->
            if (switched) scope.launch { callbacks.onCameraFacing(isFrontFacing) }
        }
    }

    /**
     * camera-off-cleanup — broadcast this device's current camera on/off
     * state ([localCameraOn]) to every other peer over the opaque SFU
     * `signal-relay`. The receiver flips this peer's roster `camOn`
     * deterministically, fixing the mute case where
     * `videoTrack.setEnabled(false)` fires no track event (and the unmute
     * case). UI-only: the signal rides the opaque relay the SFU never parses
     * (invariant #2); it touches no SFrame / seed / MLS state. No-op if
     * signaling is not open.
     */
    private fun broadcastMediaState() {
        signaling?.sendSignalRelayToCall(composeMediaState(localCameraOn))
    }

    /**
     * Current mid-call camera-on lifecycle state (ADR-0032 piece B).
     */
    fun getVideoPublishState(): VideoPublishState = videoPublishState

    /**
     * Turn the camera ON mid-call (ADR-0032 piece B). When the group call
     * started audio-only there is no video sender, so this ACQUIRES the
     * camera, adds the track (SFrame-sealed exactly like the audio sender),
     * and renegotiates with the SFU. Idempotent + re-entrant-safe:
     *   * a live video sender already exists -> just re-enable its track;
     *   * a capture is already in flight      -> no-op (do not double-acquire).
     *
     * Fail-safe (invariant #4): on a no-camera / capture / SFrame-arm failure
     * the just-added sender (if any) is removed and the capture torn down —
     * NEVER a half-added, plaintext video sender — and the state goes
     * [VideoPublishState.FAILED], so the caller reverts the toggle + surfaces a
     * message; the call stays up audio-only.
     *
     * Runs entirely on [scope] (single-threaded), so the perfect-negotiation
     * flags it touches need no synchronisation.
     *
     * @throws SfuCallException on a capture / SFrame-arm failure, so the caller
     *         can distinguish it from the "already publishing" no-op paths.
     */
    suspend fun publishVideo() {
        val connection = pc ?: return
        // 1. A live video sender already exists (a video call, or a prior
        //    publishVideo) -> just re-enable. No capture, no renegotiation.
        if (connection.hasLiveLocalVideoSender()) {
            connection.setCameraEnabled(true)
            callbacks.onLocalVideoTrack(connection.currentLocalVideoTrack())
            setVideoPublishState(VideoPublishState.LIVE)
            // camera-off-cleanup — re-enabling an existing sender fires no
            // track event, so signal the camera-on so peers un-freeze our tile.
            localCameraOn = true
            broadcastMediaState()
            syncSenderActivity()
            return
        }
        // 2. A capture is already in flight -> do not double-acquire.
        if (videoPublishState == VideoPublishState.STARTING) return
        // No seed -> cannot SFrame-seal, so cannot publish (invariant #4).
        // Silent (the group-call path always has a seed by the time a live
        // call's camera button is reachable).
        if (activeSeed == null) return

        setVideoPublishState(VideoPublishState.STARTING)

        // A sealed video sender whose track ended by itself: its capture is
        // dead. Release it before acquiring the camera again; the sender
        // itself is kept and reused in step 4.
        if (connection.hasSealedLocalVideoSender()) {
            localCameraCapture?.let { stale ->
                stale.stop()
                stale.dispose()
            }
            localCameraCapture = null
        }

        // 3. Capture the camera (create source + capturer, start). On any
        //    failure -> FAILED + throw; no sender has been added yet, so there
        //    is nothing half-added to clean up.
        if (!CameraVideoCapture.hasUsableCamera(applicationContext)) {
            setVideoPublishState(VideoPublishState.FAILED)
            throw SfuCallException("no camera available")
        }
        val source = WebRtcPeerConnection.newVideoSource(applicationContext)
        val eglContext = WebRtcPeerConnection.sharedEglContext()
        if (source == null || eglContext == null) {
            setVideoPublishState(VideoPublishState.FAILED)
            throw SfuCallException("could not create a video source")
        }
        val capture = CameraVideoCapture.create(applicationContext, source, eglContext, preferredCamera)
        if (capture == null) {
            setVideoPublishState(VideoPublishState.FAILED)
            throw SfuCallException("could not create the camera capture")
        }
        callbacks.onCameraFacing(capture.startsFrontFacing)
        try {
            capture.start()
        } catch (cause: CameraVideoCapture.CaptureError) {
            capture.dispose()
            setVideoPublishState(VideoPublishState.FAILED)
            throw SfuCallException("camera start failed: ${cause.message}")
        }

        // 4. Re-read pc + the CURRENT seed AFTER the (blocking) capture start
        //    — a rekey / teardown may have interleaved. Attach at the current
        //    epoch's seed.
        val connection2 = pc
        val seed = activeSeed
        if (connection2 == null || seed == null) {
            // Torn down / lost the seed mid-capture -> abandon cleanly (not a
            // user error — the call is going away).
            capture.stop()
            capture.dispose()
            setVideoPublishState(VideoPublishState.OFF)
            return
        }
        if (connection2.hasSealedLocalVideoSender()) {
            // The earlier camera track ended by itself and its sender is still
            // on the connection with its SFrame encryptor armed. One sender
            // per stream: put the fresh track on THAT sender. A second sender
            // would seal the same (leaf, stream) KID under a second encryptor
            // with its own counter (ADR-0035, ADR-0069). Nothing is added to
            // the SDP, so there is no renegotiation either.
            if (!connection2.replaceLocalVideoTrack(source)) {
                capture.stop()
                capture.dispose()
                setVideoPublishState(VideoPublishState.FAILED)
                throw SfuCallException("could not put the camera on the existing video sender")
            }
            localCameraCapture = capture
            localCameraSource = source
            // The copies followed the camera onto the new source inside
            // `replaceLocalVideoTrack` (same senders, same encryptors). One
            // that was never there, or was dropped, is added now, which is a
            // new media line and so a renegotiation.
            val copyAdded = addCameraCopies(connection2, source, seed)
            connection2.setCameraEnabled(true)
            setVideoPublishState(VideoPublishState.LIVE)
            callbacks.onLocalVideoTrack(connection2.currentLocalVideoTrack())
            localCameraOn = true
            broadcastMediaState()
            // A replaced track keeps the sender and its parameters; asked
            // again all the same, in case the platform reset the encoding.
            connection2.syncSenderCodecs()
            syncSenderActivity()
            if (copyAdded) driveRenegotiationOffer()
            return
        }
        // ADR-0067 step 2: the main camera sender as on the initial publish.
        // Send encodings can only be given when the transceiver is created,
        // so the camera gets a transceiver of its own here too (as on web)
        // and the renegotiation offer below carries it.
        connection2.addLocalVideoTrack(source, groupCamera = true)
        // Arm SFrame on ONLY the NEW video sender, at the current seed +
        // publishLeaf. This does NOT re-arm the already-armed audio sender.
        if (!connection2.armSFrameEncryptorForVideoSender(seed, publishLeaf)) {
            // Fail-closed (invariant #4): never ship a plaintext video sender.
            // Remove the just-added sender + tear the capture down; the call
            // continues audio-only.
            connection2.removeLocalVideoTrack()
            capture.stop()
            capture.dispose()
            setVideoPublishState(VideoPublishState.FAILED)
            throw SfuCallException("SFrame arm on the video sender failed (invariant #4)")
        }
        // The camera is now published + sealed. Hold the capture for teardown
        // in [hangUp].
        localCameraCapture = capture
        localCameraSource = source
        // Its copies (ADR-0070 steps 1 and 5), each added and sealed in one
        // step, before the renegotiation offer that carries their media lines.
        addCameraCopies(connection2, source, seed)
        syncSenderActivity()

        // 5. Live: surface the self-preview track + report the state.
        setVideoPublishState(VideoPublishState.LIVE)
        callbacks.onLocalVideoTrack(connection2.currentLocalVideoTrack())
        // camera-off-cleanup — the new video sender's track event covers
        // peers already subscribed after renegotiation, but broadcast the
        // camera-on too so the state is authoritative + a late newcomer's
        // peer-join re-announce reflects it.
        localCameraOn = true
        broadcastMediaState()

        // 6. Renegotiate so the SFU learns about the new video m-line.
        driveRenegotiationOffer()
    }

    /**
     * Drive a mid-call renegotiation offer (ADR-0032 piece B). SERIALIZE (no
     * rollback): if we are not ready — the initial publish is not yet answered
     * ([negotiationReady]), our own offer is in flight ([makingOffer]), we are
     * mid-drain of SFU offers ([drainingOffers]), or the SDP state is not
     * `STABLE` — mark [pendingRenegotiation] and return; the
     * [applyRemoteAnswer] / [drainServerOffers] tail re-drives it. Otherwise
     * build + send our offer; [makingOffer] stays set until its answer arrives.
     */
    private suspend fun driveRenegotiationOffer() {
        val connection = pc ?: return
        val sig = signaling ?: return
        if (!negotiationReady ||
            makingOffer ||
            drainingOffers ||
            connection.signalingState() != PeerConnection.SignalingState.STABLE
        ) {
            pendingRenegotiation = true
            return
        }
        pendingRenegotiation = false
        makingOffer = true
        try {
            val offer = connection.createRenegotiationOffer()
            sendOffer(connection, offer)
            // makingOffer stays true until applyRemoteAnswer clears it.
        } catch (e: Throwable) {
            makingOffer = false
            android.util.Log.w("SfuCallSession", "renegotiation offer failed", e)
            // A failed renegotiation leaves the call up as-is; the local
            // self-preview is already live.
        }
    }

    // ---- Media counters (CallMediaCounters.kt) -----------------------
    // Read and written on [scope] only.
    private var mediaCountersJob: Job? = null
    private var mediaCountersLast: MediaReading? = null
    private var mediaCountersLastAtMs: Long = 0L
    private var senderCountersLast: SenderReading? = null

    /**
     * ADR-0067 step 1: log what the receivers report, once per interval and
     * as a total when the call ends, with what each remote tile showed
     * (`LayerSwitch.kt`). Diagnostics only: counts and durations.
     */
    private fun startMediaCounters(connection: WebRtcPeerConnection) {
        mediaCountersJob?.cancel()
        mediaCountersLast = null
        mediaCountersLastAtMs = nowMs()
        senderCountersLast = null
        // The tiles of this call start from nothing.
        shownLayerLedger.reset()
        mediaCountersJob = scope.launch {
            while (isActive) {
                delay(MEDIA_COUNTERS_INTERVAL_MS)
                sampleMediaCounters(connection)
            }
        }
    }

    private suspend fun sampleMediaCounters(connection: WebRtcPeerConnection) {
        if (pc !== connection) return
        val sample = connection.sampleMediaStats() ?: return
        if (pc !== connection) return
        val at = nowMs()
        val reading = readMedia(sample.stats, sample.lightTrackIds, sample.middleTrackIds, sample.senderByMid)
        val sender = readSender(sample.stats, sample.senderByMid)
        // A sender libwebrtc refused to start is asked again, and a flag a
        // renegotiation reset is put back.
        syncSenderActivity()
        val label = MEDIA_COUNTERS_INTERVAL_LABEL
        val kbps = senderKbps(mediaCountersLast, reading, at - mediaCountersLastAtMs)
        val lines = describeDeltas(label, deltaOf(mediaCountersLast, reading)) +
            // One line for each camera sender: its size, what it sent, the
            // SFU's last word for it and whether its encoding is active.
            describeOutboundSenders(
                label,
                reading.outboundVideoSenders,
                activityOf = ::senderActivityNote,
                existing = CAMERA_SENDER_KINDS.filter { senderActivity.getValue(it).bound },
                kbpsOf = { kbps[it] },
            ) +
            listOfNotNull(sender?.let { describeSender(label, senderCountersLast, it) }) +
            // What each remote tile showed over the interval, with what is
            // asked of the SFU for that person and what it says it gives.
            describeShownPeers(
                label,
                shownLayerLedger.takeInterval(),
                wantOf = { wantsBook.qualityOf(it) },
                givenOf = { givenBook.givenOf(it) },
            )
        for (line in lines) android.util.Log.i(MEDIA_LOG_TAG, line)
        mediaCountersLast = reading
        mediaCountersLastAtMs = at
        if (sender != null) senderCountersLast = sender
    }

    /** Stop the poll and log the whole call's totals from what was read. */
    private fun stopMediaCounters() {
        mediaCountersJob?.cancel()
        mediaCountersJob = null
        senderCountersLast = null
        val last = mediaCountersLast ?: return
        mediaCountersLast = null
        val label = MEDIA_COUNTERS_TOTAL_LABEL
        // Counted from zero: the counters are cumulative from the stream's start.
        val lines = describeDeltas(label, deltaOf(null, last)) +
            describeShownPeers(
                label,
                shownLayerLedger.totals(),
                wantOf = { wantsBook.qualityOf(it) },
                givenOf = { givenBook.givenOf(it) },
            )
        for (line in lines) android.util.Log.i(MEDIA_LOG_TAG, line)
        shownLayerLedger.reset()
    }

    // ---- Signaling --------------------------------------------------

    private fun signalingCallbacks(): SfuSignalingCallbacks =
        object : SfuSignalingCallbacks {
            override fun onWelcome(
                selfPeerId: String,
                callId: String,
                roster: List<SfuPeerSnapshot>,
            ) {
                scope.launch {
                    lastInboundMs = nowMs()
                    this@SfuCallSession.selfPeerId = selfPeerId
                    wantsBook.setPeers(roster.map { it.peerId }.filter { it != selfPeerId })
                    onRosterChanged(joined = false)
                }
                callbacks.onWelcome(selfPeerId, roster)
            }

            override fun onVideoGiven(given: List<SfuVideoGiven>) {
                scope.launch { onVideoGivenFrame(given) }
            }

            override fun onVideoNeeded(main: Boolean, middle: Boolean) {
                scope.launch { onVideoNeededFrame(VideoNeeded(main = main, middle = middle)) }
            }

            override fun onPong(nonce: ULong) {
                scope.launch { lastInboundMs = nowMs() }
            }

            override fun onPeerJoined(peerId: String, accountPseudonym: String, peerTag: String) {
                scope.launch {
                    if (peerId != selfPeerId) {
                        wantsBook.addPeer(peerId)
                        onRosterChanged(joined = true)
                    }
                }
                callbacks.onPeerJoined(peerId, accountPseudonym, peerTag)
                // camera-off-cleanup — re-announce our current camera state
                // directly to the newcomer (deterministic initial-sync): its
                // freshly-created tile for us defaults camera-off, and its
                // subscribe to our still-present track would flip camOn true
                // even if we are muted, so this asserts the true state.
                // Targeted to the joiner, not the whole call, to avoid
                // re-notifying existing peers.
                signaling?.sendSignalRelayToPeer(peerId, composeMediaState(localCameraOn))
            }

            override fun onPeerLeft(peerId: String) {
                scope.launch {
                    wantsBook.removePeer(peerId)
                    // What the SFU gave of them goes with them: someone who
                    // later joins under the same id starts with nothing said.
                    if (givenBook.removePeer(peerId)) callbacks.onVideoGiven(givenBook.snapshot())
                    onRosterChanged(joined = false)
                }
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
                scope.launch {
                    // camera-off-cleanup — classify the opaque relay payload.
                    // A camera on/off media-state signal fires the typed
                    // [Callbacks.onRemoteMediaState] (so the receiver flips this
                    // peer's roster camOn even though a mute fires no track
                    // event); every other payload falls through to the opaque
                    // [Callbacks.onSignalRelay] orchestrator (the multi-joiner
                    // KeyPackage / Welcome / commit handshake). A classify
                    // failure is non-fatal — treat it as an opaque relay.
                    val cameraOn = try {
                        val env = dispatchIncomingEnvelope(bytes)
                        if (env.kind == FfiIncomingEnvelopeKind.MEDIA_STATE) {
                            env.cameraOn
                        } else {
                            null
                        }
                    } catch (e: Throwable) {
                        null
                    }
                    if (cameraOn != null) {
                        callbacks.onRemoteMediaState(from, cameraOn)
                    } else {
                        callbacks.onSignalRelay(from, bytes)
                    }
                }
            }

            override fun onSdpAnswer(sdp: String) {
                scope.launch { applyRemoteAnswer(sdp) }
            }

            override fun onSdpOffer(sdp: String) {
                offerQueue.addLast(sdp)
                scope.launch { drainServerOffers() }
            }

            override fun onServerError(code: SfuErrorCode, detail: String) {
                if (code == SfuErrorCode.CALL_ENDED) {
                    // The SFU's answer in place of the Welcome: the room stood
                    // empty past its grace before this device arrived. Not a
                    // failure of this device's; the host closes the chat's
                    // record and says the call ended.
                    scope.launch {
                        endedByServer = true
                        fail()
                    }
                    return
                }
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
            return
        }
        // The senders' codecs are negotiated now: their SFrame encryptors need
        // them to leave the right codec header in the clear (ADR-0071).
        connection.syncSenderCodecs()
        // A flag this negotiation reset on a sender is put back.
        syncSenderActivity()
        // The initial publish (or a mid-call renegotiation) is answered (piece
        // B): clear our in-flight flag + arm future renegotiations, then either
        // send a camera-on that queued while we were mid-offer, or drain the
        // SFU subscribe-offers that waited in the queue.
        makingOffer = false
        negotiationReady = true
        if (pendingRenegotiation) {
            driveRenegotiationOffer()
        } else {
            drainServerOffers()
        }
    }

    private suspend fun drainServerOffers() {
        if (drainingOffers) return
        // Serialize (piece B): our own publish / renegotiation offer is in
        // flight — leave the SFU offers queued; applyRemoteAnswer re-drives
        // this once the answer clears `makingOffer` (no rollback — our
        // offer->answer completes fully before any SFU subscribe-offer applies).
        if (makingOffer) return
        drainingOffers = true
        try {
            while (offerQueue.isNotEmpty()) {
                val sdp = offerQueue.removeFirst()
                val connection = pc ?: break
                val sig = signaling ?: break
                try {
                    val answer = connection.createAnswer(sdp)
                    sig.sendSdpAnswer(answer)
                    // A renegotiation can change what a sender sends as
                    // (ADR-0071) and reset an encoding's flag.
                    connection.syncSenderCodecs()
                    syncSenderActivity()
                } catch (e: Throwable) {
                    // A single bad renegotiation drops that subscription;
                    // the call stays up. Continue draining.
                }
            }
        } finally {
            drainingOffers = false
        }
        // A camera-on that arrived while we were draining queued a pending
        // renegotiation -> drive it now that the queue is clear.
        if (pendingRenegotiation) {
            driveRenegotiationOffer()
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
            PeerConnection.PeerConnectionState.CONNECTED -> {
                if (state == State.CONNECTING) setState(State.CONNECTED)
                // Say again what this device shows, now that media flows.
                wantsSender.resend()
            }
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

    /** One keepalive evaluation, every [SFU_KEEPALIVE_TICK_MS] on [scope]. */
    private fun onKeepaliveTick() {
        if (state == State.FAILED || state == State.CLOSED) return
        val now = nowMs()
        when (sfuKeepaliveAction(now, lastInboundMs, lastPingMs)) {
            SfuKeepaliveAction.PING -> {
                pingNonce += 1u
                signaling?.sendPing(pingNonce)
                lastPingMs = now
            }
            SfuKeepaliveAction.DEAD -> {
                android.util.Log.w(
                    "SfuCallSession",
                    "SfuCallSession: keepalive dead (no pong for ${(now - lastInboundMs) / 1000} s); failing the call",
                )
                fail()
            }
            SfuKeepaliveAction.WAIT -> Unit
        }
    }

    private companion object {
        /** SFrame overlap window in ms — mirrors the web `SFRAME_OVERLAP_MS`
         *  and the Rust core `OVERLAP_DEFAULT_MS` (ADR-0008 / ADR-0029). */
        const val OVERLAP_MS = 1000L

        /** Extra margin before pruning so the window fully elapses on the
         *  media threads (mirrors the web worker's `overlapMs + 200`). */
        const val PRUNE_MARGIN_MS = 200L

        /** The log tag of the media-counter lines. */
        const val MEDIA_LOG_TAG = "SfuCallSession"
    }
}
