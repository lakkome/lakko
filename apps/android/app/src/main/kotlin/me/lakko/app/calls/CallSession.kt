// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.security.SecureRandom
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallMediaFlags
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody
import uniffi.lakko_ffi_uniffi.composeCallSignaling
import uniffi.lakko_ffi_uniffi.parseCallSignaling

/**
 * 1×1 P2P call session on Android (Phase 3a10
 * scaffolding).
 *
 * Mirror of:
 *   * `apps/ios/Lakko/Calls/CallSession.swift`
 *   * `apps/web/src/calls/call-session.ts`
 *
 * ## Phase 3a10 scope
 *
 * Signaling-only today — the real `org.webrtc:google-webrtc`
 * pod integration lands in 3a10-followup. The UI surface
 * (call button + state overlay in
 * `apps/android/.../ui/chat/ChatScreen.kt`) is fully wired
 * against this class; once the WebRTC stack integrates,
 * only `dial` / `acceptCall` need to grow the live
 * media-stream branches.
 */
public class CallSession(
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
        }
    }

    // ----------------------------------------------------------------
    // Outbound
    // ----------------------------------------------------------------

    /**
     * Outbound side: dial. In 3a10 scaffolding this
     * immediately emits the Offer with a placeholder SDP
     * the receiver-side WebRTC stack accepts but produces
     * no audio. Followup replaces with real
     * `PeerConnection.createOffer`.
     */
    public fun dial(audio: Boolean = true, video: Boolean = false) {
        if (mutableState.value != WebCallState.Idle) return
        isOfferer = true
        mutableState.value = WebCallState.Outgoing
        try {
            val bytes = composeCallSignaling(
                FfiCallSignalingBody.Offer(
                    callId = callId,
                    sdp = placeholderOfferSdp(),
                    createdAtMs = System.currentTimeMillis().toULong(),
                    media = FfiCallMediaFlags(audio = audio, video = video),
                ),
            )
            onOutboundSignaling(bytes)
        } catch (cause: Throwable) {
            android.util.Log.w("CallSession", "dial compose failed", cause)
            endLocal(WebCallEndReason.Error)
        }
    }

    /** Outbound or active call teardown. */
    public fun hangUp(reason: WebCallEndReason = WebCallEndReason.HungUp) {
        endLocal(reason)
    }

    /** Inbound side: accept the ringing call. */
    public fun acceptCall() {
        if (mutableState.value != WebCallState.Ringing || isOfferer) return
        try {
            val bytes = composeCallSignaling(
                FfiCallSignalingBody.Answer(
                    callId = callId,
                    sdp = placeholderAnswerSdp(),
                ),
            )
            onOutboundSignaling(bytes)
            mutableState.value = WebCallState.Connecting
            // Phase 3a10 scaffolding — synthesise
            // Connected. Real ICE pair lands in followup.
            mutableState.value = WebCallState.Connected
        } catch (cause: Throwable) {
            android.util.Log.w("CallSession", "accept compose failed", cause)
            endLocal(WebCallEndReason.Error)
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
     *  machine. Caller has already verified call_id
     *  matches this session. */
    public fun applyIncomingSignaling(body: FfiCallSignalingBody) {
        when (body) {
            is FfiCallSignalingBody.Offer -> {
                if (mutableState.value != WebCallState.Idle) return
                isOfferer = false
                mutableState.value = WebCallState.Ringing
            }
            is FfiCallSignalingBody.Answer -> {
                if (mutableState.value != WebCallState.Outgoing) return
                mutableState.value = WebCallState.Connecting
                // Phase 3a10 scaffolding — synthesise
                // Connected.
                mutableState.value = WebCallState.Connected
            }
            is FfiCallSignalingBody.IceCandidate -> {
                // Phase 3a10 scaffolding — no-op until pod
                // lands.
            }
            is FfiCallSignalingBody.End -> {
                if (mutableState.value == WebCallState.Done) return
                endRemote(toReason(body.reason))
            }
        }
    }

    // ----------------------------------------------------------------
    // Internal
    // ----------------------------------------------------------------

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
        mutableState.value = WebCallState.Done
    }

    private fun endRemote(reason: WebCallEndReason) {
        if (mutableState.value == WebCallState.Done) return
        mutableEndReason.value = reason
        mutableState.value = WebCallState.Done
    }

    private fun placeholderOfferSdp(): String = listOf(
        "v=0",
        "o=- 1 1 IN IP4 0.0.0.0",
        "s=lakko-3a10-scaffold",
        "t=0 0",
        "m=audio 9 UDP/TLS/RTP/SAVPF 111",
        "a=rtpmap:111 opus/48000/2",
        "a=mid:audio",
        "",
    ).joinToString("\n")

    private fun placeholderAnswerSdp(): String = listOf(
        "v=0",
        "o=- 2 2 IN IP4 0.0.0.0",
        "s=lakko-3a10-scaffold",
        "t=0 0",
        "m=audio 9 UDP/TLS/RTP/SAVPF 111",
        "a=rtpmap:111 opus/48000/2",
        "a=mid:audio",
        "",
    ).joinToString("\n")

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

/** Phase 3a10 — convenience wrapper. Caller passes raw
 *  bytes; returns the typed body. */
public fun parseCallSignalingBytes(bytes: ByteArray): FfiCallSignalingBody =
    parseCallSignaling(bytes)
