// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.webrtc.RtpParameters

/**
 * The group call's camera sender in three temporal layers (ADR-0067 step 2).
 *
 * Two frames in three are then referenced by no later base-layer frame, so one
 * lost in a burst of packet loss costs that frame rather than the picture until
 * a keyframe. The SFU forwards packets unchanged and relays the frame
 * descriptor that tells the receiver which frames depend on which, so nothing
 * else changes. The twin of the web `camera-sender.ts`.
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

/** The single send encoding a new camera transceiver is created with. */
internal fun cameraSendEncoding(): RtpParameters.Encoding =
    RtpParameters.Encoding(null, true, null).apply {
        scalabilityMode = CAMERA_SCALABILITY_MODE
    }

/**
 * Add the camera with temporal layers, falling back to a plain add when the
 * platform refuses the mode: the call works either way, only less well under
 * loss.
 *
 * libwebrtc 125's Java `addTransceiver` throws `IllegalStateException` when
 * the native call refuses (an encoding no negotiable codec supports is one
 * such refusal), so a throw from [addLayered] is the refusal signal.
 *
 * @param addLayered adds the track through a transceiver whose send encoding
 *   is [cameraSendEncoding] and returns its sender.
 * @param addPlain adds the track the way `addTrack` would.
 * @param onRefused told why [addLayered] failed, for the log line.
 */
internal fun <S> addCameraSender(
    addLayered: () -> S,
    addPlain: () -> S,
    onRefused: (RuntimeException) -> Unit = {},
): S =
    try {
        addLayered()
    } catch (cause: RuntimeException) {
        onRefused(cause)
        addPlain()
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
 * sender came from `addTrack`, which takes no encodings — the mid-call
 * camera-on, which may reuse a video transceiver the SFU already negotiated.
 * Best effort: a refusal keeps one layer and the call goes on.
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
