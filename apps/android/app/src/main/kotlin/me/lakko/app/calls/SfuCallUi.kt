// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import org.webrtc.VideoTrack
import uniffi.lakko_ffi_uniffi.callSafetyNumber

/**
 * Chat-UI adapter for the Android native [SfuCallSession] — the Kotlin
 * twin of the iOS `SfuCallUi.swift` and the web `createSfuCallUi`.
 *
 * Binds the session's callbacks onto a [CallUiState] so the existing
 * `CallScreen` renders a live SFU roster with no per-call-type branching
 * in the view:
 *   * lifecycle [SfuCallSession.State] -> [CallUiState.CallPhase] (+ the
 *     E2EE pill flips active on `CONNECTED`, since the SFrame seed is
 *     armed at connect),
 *   * `welcome` / `peer-joined` / `peer-left` -> the roster,
 *   * a remote video track -> the sole-remote tile (per-tile N-way video
 *     is a follow-up; audio plays out automatically).
 *
 * The caller owns deriving the inputs (the JWT via [CallJwtClient], the
 * seed via `Group.exportSecret("lakko/call/v1", callId, 32)`, the leaf
 * via `group.ownLeafIndex`, the SFU base URL from config) and passes
 * them in the [SfuCallSession.Config]; this adapter is pure glue.
 *
 * The callbacks capture the [CallUiState] + the name resolver directly
 * (not `this`), so the session — assigned last in `init` — has no
 * init-order chicken-and-egg and there is no retain reference back to
 * the adapter.
 */
class SfuCallUi(
    applicationContext: Context,
    config: SfuCallSession.Config,
    scope: CoroutineScope,
    private val ui: CallUiState,
    localDisplayName: String = "",
    /** The name on a remote tile (`CallTileName.kt`), empty while the peer is
     *  unnamed. The default knows nobody; it never shows the SFU pseudonym,
     *  which is a piece of the account id. */
    resolveDisplayName: (peerId: String, accountPseudonym: String, peerTag: String) -> String =
        { _, _, _ -> "" },
) {
    /** The underlying session (for direct control if needed). */
    val session: SfuCallSession

    /** Reads who is speaking while the call is connected. */
    private val speaking: SpeakingMonitor

    private val _videoPublishState =
        MutableStateFlow(SfuCallSession.VideoPublishState.OFF)

    /** ADR-0032 piece B — mid-call camera-on lifecycle, mirrored from the
     *  session's `onVideoPublishState`. The UI can observe it for a spinner /
     *  failure surface; the primary fail-safe is [publishVideo] throwing. */
    val videoPublishState: StateFlow<SfuCallSession.VideoPublishState> =
        _videoPublishState.asStateFlow()

    init {
        // Canonical call safety number derived from the shared seed — the
        // same string every participant derives, shown in the E2EE pill
        // for an out-of-band man-in-the-middle check. Display-only +
        // non-critical: a derivation failure (only on a wrong-length seed,
        // which the MLS exporter never emits) must never break the call.
        val safetyNumber: String? =
            config.seed?.let { seed ->
                try {
                    callSafetyNumber(seed)
                } catch (e: Exception) {
                    null
                }
            }

        // Sticky last-known camera state per remote roster tile (keyed on
        // "peer-<peerId>"). Closes the join-time reorder race: a newcomer
        // subscribing to an already camera-muted peer still receives that
        // peer's present-but-disabled video track, so onRemoteVideoTrack fires
        // and would flip camOn back on. onRemoteMediaState is authoritative and
        // records the true state here; the track-arrival path defers to it so a
        // known-muted peer is never wrongly shown as camera-on. Captured by the
        // callbacks directly (like `ui` — not `this`); one adapter per call, and
        // both callbacks are delivered on the same session scope, so no extra
        // synchronisation is needed and per-call lifetime handles teardown.
        val lastKnownCameraOn = mutableMapOf<String, Boolean>()

        // Who is speaking (`docs/ux/group-call-grid.md` section 7.3), read on
        // this device from the session's own connection. The session is
        // assigned last, so the reader goes through a holder.
        var sessionRef: SfuCallSession? = null
        val monitor = SpeakingMonitor(
            scope = scope,
            ui = ui,
            sample = { sessionRef?.sampleAudioLevels() },
        )
        speaking = monitor

        val callbacks = SfuCallSession.Callbacks(
            onStateChange = { state ->
                when (state) {
                    SfuCallSession.State.CONNECTING ->
                        ui.setPhase(CallUiState.CallPhase.Connecting)
                    SfuCallSession.State.CONNECTED -> {
                        ui.setPhase(CallUiState.CallPhase.Connected)
                        // SFrame is armed at connect, so media is E2E
                        // encrypted the moment we are connected. The safety
                        // number (from the same shared seed) lets
                        // participants verify the E2EE root key out-of-band.
                        ui.setE2ee(
                            CallUiState.E2eeState(
                                active = true,
                                shortFingerprint = safetyNumber,
                            )
                        )
                        monitor.start()
                    }
                    SfuCallSession.State.FAILED,
                    SfuCallSession.State.CLOSED -> {
                        monitor.stop()
                        // The call's tracks are gone with its connection.
                        ui.clearRemoteLightVideoTracks()
                        ui.setPhase(CallUiState.CallPhase.Ended(System.currentTimeMillis()))
                    }
                    SfuCallSession.State.IDLE -> Unit
                }
            },
            onWelcome = { selfPeerId, roster ->
                val local = localParticipant(localDisplayName, config.audio, config.video)
                val remotes = roster
                    .filter { it.peerId != selfPeerId }
                    .map {
                        remoteParticipant(
                            resolveDisplayName(it.peerId, it.accountPseudonym, it.peerTag),
                            it.peerId,
                        )
                    }
                ui.setParticipants(listOf(local) + remotes)
            },
            onPeerJoined = { peerId, accountPseudonym, peerTag ->
                ui.participantJoined(
                    remoteParticipant(
                        resolveDisplayName(peerId, accountPseudonym, peerTag),
                        peerId,
                    )
                )
            },
            onPeerLeft = { peerId ->
                val id = participantId(peerId)
                // Drop the peer's video track first so its tile stops
                // rendering a frozen last frame, then remove the roster entry.
                ui.setRemoteVideoTrack(null, id)
                // participantLeft also drops the peer's light companion.
                ui.participantLeft(id)
                lastKnownCameraOn.remove(id)
            },
            onRemoteVideoTrack = { track, streamId ->
                // N-way remote video. The MSID stream id is the source peer
                // id for a peer's main track and that id plus `~light` for its
                // light companion (`LightStream.kt`).
                //
                // The main track is keyed on the peer's roster id
                // (`peer-<peerId>`) so each tile renders its own peer, and
                // sets that participant's camera flag so the tile's video
                // surface (gated on `camOn`) renders.
                //
                // Join-race guard: a newcomer subscribing to an already
                // camera-muted peer still receives that peer's present-but-
                // disabled video track, so this path fires even for a muted
                // peer and would wrongly flip `camOn` back on. onRemoteMediaState
                // is authoritative and records the true state in
                // [lastKnownCameraOn]; the main track DEFERS to it.
                //
                // The companion goes to its own map and nothing else: no
                // participant, no tile, no camera flag.
                applyRemoteVideoTrack(ui, track, streamId, lastKnownCameraOn)
            },
            onLocalVideoTrack = { track ->
                // Local camera (video call, or a mid-call publishVideo) ->
                // self-preview tile.
                ui.setLocalVideoTrack(track)
            },
            onVideoPublishState = { s ->
                // Mirror the mid-call camera-on lifecycle (piece B) onto both
                // the adapter's own flow AND the shared CallUiState, so the
                // CallScreen collects ONE surface for the camera-button spinner
                // + the self-tile hint (piece B2a).
                _videoPublishState.value = s
                ui.setVideoPublishState(
                    when (s) {
                        SfuCallSession.VideoPublishState.OFF ->
                            CallUiState.VideoPublishState.Off
                        SfuCallSession.VideoPublishState.STARTING ->
                            CallUiState.VideoPublishState.Starting
                        SfuCallSession.VideoPublishState.LIVE ->
                            CallUiState.VideoPublishState.Live
                        SfuCallSession.VideoPublishState.FAILED ->
                            CallUiState.VideoPublishState.Failed
                    }
                )
            },
            onRemoteMediaState = { peerId, cameraOn ->
                // camera-off-cleanup — a remote peer signalled its camera
                // state over the opaque SFU relay. Flip that participant's
                // camOn so its tile stops rendering a frozen last frame on
                // mute (and re-renders on unmute). Same target as the
                // onRemoteVideoTrack camera flag, but driven by an explicit
                // signal so a mute — which fires no track event — is handled.
                // `peerId` == the relay sender's SFU peer id == the roster
                // tile key ("peer-<peerId>"); setParticipantCamera no-ops on
                // an unknown id (the media-state raced ahead of peer-joined).
                // Record the authoritative state so a later track ARRIVAL for
                // this peer defers to it (the join-race guard above).
                val id = participantId(peerId)
                lastKnownCameraOn[id] = cameraOn
                ui.setParticipantCamera(id, cameraOn)
            },
            onCameraFacing = { front -> ui.setCameraFrontFacing(front) },
            onServerError = { _, _ -> },
        )
        session = SfuCallSession(
            applicationContext = applicationContext,
            config = config,
            scope = scope,
            callbacks = callbacks,
        )
        sessionRef = session
    }

    /** Open the SFU connection + publish (delegates to the session). */
    suspend fun connect() {
        session.connect()
    }

    /** Tear down (the session drives the UI to `Ended` via its state
     *  callback). Idempotent. */
    fun hangUp() {
        speaking.stop()
        session.hangUp()
    }

    /**
     * Rotate the call key on a mid-call MLS epoch change (G-18, ADR-0029).
     * The caller re-derives the per-call seed at the new epoch and passes
     * it; every installed SFrame encryptor / decryptor rotates with an
     * overlap window so in-flight frames still open. No-op if unchanged.
     */
    fun rekey(newSeed: ByteArray) {
        session.rekey(newSeed)
    }

    /** Mute / unmute the local mic — drives the real track + mirrors the
     *  store's local-participant flag. */
    fun setMicrophoneEnabled(on: Boolean) {
        session.setMicrophoneEnabled(on)
        ui.setMic(on)
    }

    /** Enable / disable the local camera — drives the real track + mirrors
     *  the store's local-participant flag. Only meaningful once a video track
     *  exists (a video call, or after a mid-call [publishVideo]). */
    fun setCameraEnabled(on: Boolean) {
        session.setCameraEnabled(on)
        ui.setCamera(on)
    }

    /** Use the camera the person chose in the call's Settings sheet, or the
     *  default one for `null` ([SfuCallSession.selectCamera]). */
    fun selectCamera(cameraName: String?) {
        session.selectCamera(cameraName)
    }

    /**
     * ADR-0032 piece B — turn the camera ON mid-call (a group call that
     * started audio-only). Acquires the camera, SFrame-seals the new sender,
     * and renegotiates. Idempotent + in-flight-guarded. Throws on a capture /
     * arm failure so the caller reverts the toggle (fail-safe; the call stays
     * up audio-only, invariant #4). Delegates to [SfuCallSession.publishVideo].
     */
    suspend fun publishVideo() {
        session.publishVideo()
    }

    internal companion object {
        /** The roster id of a remote peer (`peer-<peerId>`); the MSID stream
         *  id == the source peer id per the SFU contract. Internal so the
         *  host's name resolver can address the same tile. */
        internal fun participantId(peerId: String): String = remoteParticipantId(peerId)

        fun localParticipant(
            name: String,
            audio: Boolean,
            video: Boolean,
        ): CallUiState.Participant =
            CallUiState.Participant(
                id = "local",
                displayName = name,
                isLocal = true,
                micOn = audio,
                camOn = video,
                quality = CallUiState.QualityTier.Good,
                speaking = false,
            )

        fun remoteParticipant(
            name: String,
            peerId: String,
        ): CallUiState.Participant =
            CallUiState.Participant(
                id = participantId(peerId),
                displayName = name,
                isLocal = false,
                micOn = true,
                camOn = false,
                quality = CallUiState.QualityTier.Good,
                speaking = false,
            )
    }
}
