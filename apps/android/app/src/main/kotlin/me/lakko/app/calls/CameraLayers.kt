// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.webrtc.RtpParameters

/**
 * The group call's camera sender (ADR-0067 steps 2 and 5).
 *
 * The camera is sent in three temporal layers: two frames in three are then
 * referenced by no later base-layer frame, so one lost in a burst of packet
 * loss costs that frame rather than the picture until a keyframe. The SFU
 * forwards packets unchanged and relays the frame descriptor that tells the
 * receiver which frames depend on which.
 *
 * It is also sent twice, as two simulcast layers: the full picture and a light
 * one a quarter of the size. The SFU gives every viewer both as two tracks, and
 * the viewer's tile shows the light one while the full one stands still
 * (`LightStream.kt`, `LayerSwitch.kt`). The twin of the web `camera-sender.ts`.
 *
 * Only the SFU session uses this. The one-to-one call keeps its plain
 * `addTrack`, and the microphone is added as before.
 *
 * The decisions live here, over lambdas, so the host JVM tests can run them
 * without a native libwebrtc: `RtpParameters.Encoding` is a plain Java class,
 * while a peer connection and a sender are not constructible off-device.
 */

/** The camera's encoding: one spatial layer, three temporal layers. */
internal const val CAMERA_SCALABILITY_MODE: String = "L1T3"

/**
 * The light simulcast layer's RTP stream id. The SFU forwards this layer to
 * every viewer as the publisher's light companion track, and matches it by
 * this exact name.
 */
internal const val CAMERA_RID_LOW: String = "q"

/** The full-resolution simulcast layer's RTP stream id. */
internal const val CAMERA_RID_FULL: String = "f"

/** The light layer is the camera picture scaled down by this, each side. */
internal const val CAMERA_LOW_SCALE_DOWN: Double = 4.0

/** The light layer's bitrate ceiling, bits a second. */
internal const val CAMERA_LOW_MAX_BITRATE_BPS: Int = 150_000

/**
 * Whether the camera is sent as two simulcast layers. The light layer costs a
 * publisher about 150 kbit/s.
 */
internal const val CAMERA_SIMULCAST_ENABLED: Boolean = false

/** The single send encoding sent when simulcast is refused or switched off. */
internal fun cameraSendEncoding(): RtpParameters.Encoding =
    RtpParameters.Encoding(null, true, null).apply {
        scalabilityMode = CAMERA_SCALABILITY_MODE
    }

/**
 * The camera's two simulcast encodings, light layer first: rid `q` (a quarter
 * of the size each side, at most 150 kbit/s) and rid `f` (the full picture).
 * [layered] asks for temporal layers on both.
 */
internal fun cameraSimulcastEncodings(layered: Boolean = true): List<RtpParameters.Encoding> {
    val low = RtpParameters.Encoding(CAMERA_RID_LOW, true, CAMERA_LOW_SCALE_DOWN).apply {
        maxBitrateBps = CAMERA_LOW_MAX_BITRATE_BPS
        if (layered) scalabilityMode = CAMERA_SCALABILITY_MODE
    }
    val full = RtpParameters.Encoding(CAMERA_RID_FULL, true, null).apply {
        if (layered) scalabilityMode = CAMERA_SCALABILITY_MODE
    }
    return listOf(low, full)
}

/** One way of creating the camera transceiver, tried in order. */
internal class CameraSenderAttempt(
    /** What is asked for, for the log line when it is refused. */
    val what: String,
    /** The send encodings the transceiver is created with. */
    val encodings: List<RtpParameters.Encoding>,
)

/**
 * What [addCameraSender] tries, in order: two simulcast layers each in temporal
 * layers; two simulcast layers with the platform's default temporal structure;
 * one encoding in temporal layers. With [simulcast] off only the last.
 */
internal fun cameraSenderAttempts(
    simulcast: Boolean = CAMERA_SIMULCAST_ENABLED,
): List<CameraSenderAttempt> = buildList {
    if (simulcast) {
        add(CameraSenderAttempt("simulcast with temporal layers", cameraSimulcastEncodings(true)))
        add(CameraSenderAttempt("simulcast", cameraSimulcastEncodings(false)))
    }
    add(CameraSenderAttempt("temporal layers", listOf(cameraSendEncoding())))
}

/** How the camera sender ended up being created, for the log line. */
internal class CameraSenderResult<S>(
    val sender: S,
    /** [CameraSenderAttempt.what] of the attempt that was accepted, or `null`
     *  when every one was refused and the track was added plainly. */
    val accepted: String?,
)

/**
 * Add the camera as a new sender. Send encodings can only be given when the
 * transceiver is created, so both the first publish and the mid-call camera-on
 * come through here. Each [cameraSenderAttempts] entry is tried in order; when
 * all are refused the track is added plainly. The call works at every step,
 * only less well under loss.
 *
 * libwebrtc 125's Java `addTransceiver` throws `IllegalStateException` when
 * the native call refuses (an encoding no negotiable codec supports is one
 * such refusal), so a throw from [addWithEncodings] is the refusal signal.
 *
 * @param addWithEncodings adds the track through a transceiver created with
 *   the given send encodings and returns its sender.
 * @param addPlain adds the track the way `addTrack` would.
 * @param onRefused told which attempt failed and why, for the log line.
 */
internal fun <S> addCameraSender(
    addWithEncodings: (List<RtpParameters.Encoding>) -> S,
    addPlain: () -> S,
    simulcast: Boolean = CAMERA_SIMULCAST_ENABLED,
    onRefused: (what: String, cause: RuntimeException) -> Unit = { _, _ -> },
): CameraSenderResult<S> {
    for (attempt in cameraSenderAttempts(simulcast)) {
        try {
            return CameraSenderResult(addWithEncodings(attempt.encodings), attempt.what)
        } catch (cause: RuntimeException) {
            onRefused(attempt.what, cause)
        }
    }
    return CameraSenderResult(addPlain(), null)
}

/** What [requestCameraLayers] did, for the log line and the tests. */
internal enum class CameraLayersOutcome {
    /** The first encoding now asks for [CAMERA_SCALABILITY_MODE]. */
    APPLIED,

    /** The first encoding already asked for it; nothing was sent. */
    ALREADY_SET,

    /** The sender reported no encoding; nothing was sent. */
    NO_ENCODING,

    /** The platform refused; the sender keeps one layer. */
    REFUSED,
}

/**
 * Ask an already-added camera sender for temporal layers. Used where the
 * sender came from `addTrack`, which takes no encodings: the last fallback
 * of [addCameraSender]. Best effort: a refusal keeps one layer and the call goes on.
 *
 * @param encodings the encodings of the parameters the sender just reported;
 *   the first one is changed in place.
 * @param apply hands those parameters back to the sender and reports whether
 *   it accepted them (`RtpSender.setParameters`).
 */
internal fun requestCameraLayers(
    encodings: List<RtpParameters.Encoding>,
    apply: () -> Boolean,
): CameraLayersOutcome {
    val first = encodings.firstOrNull() ?: return CameraLayersOutcome.NO_ENCODING
    if (first.scalabilityMode == CAMERA_SCALABILITY_MODE) return CameraLayersOutcome.ALREADY_SET
    first.scalabilityMode = CAMERA_SCALABILITY_MODE
    val accepted = try {
        apply()
    } catch (cause: RuntimeException) {
        false
    }
    return if (accepted) CameraLayersOutcome.APPLIED else CameraLayersOutcome.REFUSED
}
