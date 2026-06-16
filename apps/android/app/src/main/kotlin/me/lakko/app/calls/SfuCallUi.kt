// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import kotlinx.coroutines.CoroutineScope
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
    localDisplayName: String = "You",
    resolveDisplayName: (peerId: String, accountPseudonym: String) -> String = { _, pseudonym ->
        if (pseudonym.isEmpty()) "Peer" else pseudonym.take(8)
    },
) {
    /** The underlying session (for direct control if needed). */
    val session: SfuCallSession

    init {
        // Canonical call safety number derived from the shared seed — the
        // same string every participant derives, shown in the E2EE pill
        // for an out-of-band man-in-the-middle check. Display-only +
        // non-critical: a derivation failure (only on a wrong-length seed,
        // which the MLS exporter never emits) must never break the call.
        val safetyNumber: String? =
            try {
                callSafetyNumber(config.seed)
            } catch (e: Exception) {
                null
            }

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
                    }
                    SfuCallSession.State.FAILED,
                    SfuCallSession.State.CLOSED ->
                        ui.setPhase(CallUiState.CallPhase.Ended(System.currentTimeMillis()))
                    SfuCallSession.State.IDLE -> Unit
                }
            },
            onWelcome = { selfPeerId, roster ->
                val local = localParticipant(localDisplayName, config.audio, config.video)
                val remotes = roster
                    .filter { it.peerId != selfPeerId }
                    .map {
                        remoteParticipant(
                            resolveDisplayName(it.peerId, it.accountPseudonym),
                            it.peerId,
                        )
                    }
                ui.setParticipants(listOf(local) + remotes)
            },
            onPeerJoined = { peerId, accountPseudonym ->
                ui.participantJoined(
                    remoteParticipant(resolveDisplayName(peerId, accountPseudonym), peerId)
                )
            },
            onPeerLeft = { peerId ->
                ui.participantLeft(participantId(peerId))
            },
            onRemoteVideoTrack = { track, _ ->
                // Sole-remote video (the 2-party case). Per-tile N-way
                // video needs a per-participant track map — a follow-up.
                ui.setRemoteVideoTrack(track)
            },
            onLocalVideoTrack = { track ->
                // Local camera (video call) -> self-preview tile.
                ui.setLocalVideoTrack(track)
            },
            onServerError = { _, _ -> },
        )
        session = SfuCallSession(
            applicationContext = applicationContext,
            config = config,
            scope = scope,
            callbacks = callbacks,
        )
    }

    /** Open the SFU connection + publish (delegates to the session). */
    suspend fun connect() {
        session.connect()
    }

    /** Tear down (the session drives the UI to `Ended` via its state
     *  callback). Idempotent. */
    fun hangUp() {
        session.hangUp()
    }

    /** Mute / unmute the local mic — drives the real track + mirrors the
     *  store's local-participant flag. */
    fun setMicrophoneEnabled(on: Boolean) {
        session.setMicrophoneEnabled(on)
        ui.setMic(on)
    }

    /** Enable / disable the local camera — drives the real track + mirrors
     *  the store's local-participant flag. Only meaningful for a video
     *  call (the video track is added at connect when `config.video`). */
    fun setCameraEnabled(on: Boolean) {
        session.setCameraEnabled(on)
        ui.setCamera(on)
    }

    private companion object {
        fun participantId(peerId: String): String = "peer-$peerId"

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
                handRaised = false,
                quality = CallUiState.QualityTier.Good,
                speaking = false,
                screenSharing = false,
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
                handRaised = false,
                quality = CallUiState.QualityTier.Good,
                speaking = false,
                screenSharing = false,
            )
    }
}
