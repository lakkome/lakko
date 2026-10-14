// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.webrtc.RtpParameters

/**
 * The group call's camera senders (ADR-0067 step 2, ADR-0070 steps 1 and 5):
 * the twin of the web `camera-sender.ts`.
 *
 * The camera is sent as up to three ORDINARY senders, each on a transceiver
 * of its own and sealed under its own SFrame stream index:
 *
 *   * the **main** one, in three temporal layers (two frames in three are
 *     referenced by no later base-layer frame, so one lost in a burst of
 *     packet loss costs that frame rather than the picture until a keyframe);
 *   * the **light** one, the same camera scaled to a quarter, at most
 *     150 kbit/s, which every viewer receives and shows while the picture it
 *     watches stands still;
 *   * the **middle** one, half the size, at most 500 kbit/s, sent only from
 *     three people in the call on, which a viewer's grid tile watches in
 *     place of the main one.
 *
 * No simulcast: which of them is sent is this client's and the SFU's
 * decision, never a layer allocator's.
 *
 * Only the SFU session uses this. The one-to-one call keeps its plain
 * `addTrack`, and the microphone is added as before.
 *
 * The decisions live here, over lambdas, so the host JVM tests can run them
 * without a native libwebrtc: `RtpParameters.Encoding` is a plain Java class,
 * while a peer connection and a sender are not constructible off-device.
 */

/** The main camera's encoding: one spatial layer, three temporal layers. */
internal const val CAMERA_SCALABILITY_MODE: String = "L1T3"

/** The light sender's picture is the camera's scaled down by this, each side. */
internal const val CAMERA_LIGHT_SCALE_DOWN: Double = 4.0

/** The light sender's bitrate ceiling, bits a second. */
internal const val CAMERA_LIGHT_MAX_BITRATE_BPS: Int = 150_000

/** The middle sender's picture is the camera's scaled down by this, each side. */
internal const val CAMERA_MIDDLE_SCALE_DOWN: Double = 2.0

/** The middle sender's bitrate ceiling, bits a second. */
internal const val CAMERA_MIDDLE_MAX_BITRATE_BPS: Int = 500_000

/**
 * The SFrame stream index each sender is sealed under (ADR-0035): the seal
 * KID is `(leaf << 8) | index`, so no two senders of a participant share a
 * key. Stated, never derived from the track kind. The numbers are the web
 * client's (`SFRAME_STREAM_*` in `sframe-transform.ts`); index 2 is the
 * screen share, which this client does not send.
 */
internal const val SFRAME_STREAM_AUDIO: Int = 0
internal const val SFRAME_STREAM_CAMERA: Int = 1
internal const val SFRAME_STREAM_CAMERA_LIGHT: Int = 3
internal const val SFRAME_STREAM_CAMERA_MIDDLE: Int = 4

/** One of this device's camera senders. */
internal enum class CameraSenderKind(
    /** The name in the diagnostics and in the `video-wants` frame. */
    val wireName: String,
    /** The SFrame stream index the sender is sealed under. */
    val streamIndex: Int,
) {
    MAIN("main", SFRAME_STREAM_CAMERA),
    MIDDLE("middle", SFRAME_STREAM_CAMERA_MIDDLE),
    LIGHT("light", SFRAME_STREAM_CAMERA_LIGHT),
}

/** Every camera sender, in the order the diagnostics name them. */
internal val CAMERA_SENDER_KINDS: List<CameraSenderKind> =
    listOf(CameraSenderKind.MAIN, CameraSenderKind.MIDDLE, CameraSenderKind.LIGHT)

/**
 * A copy of the camera sent beside the main sender, each an ordinary sender
 * on a transceiver of its own.
 */
internal enum class CameraCopyKind(
    val sender: CameraSenderKind,
    val scaleDown: Double,
    val maxBitrateBps: Int,
) {
    LIGHT(CameraSenderKind.LIGHT, CAMERA_LIGHT_SCALE_DOWN, CAMERA_LIGHT_MAX_BITRATE_BPS),
    MIDDLE(CameraSenderKind.MIDDLE, CAMERA_MIDDLE_SCALE_DOWN, CAMERA_MIDDLE_MAX_BITRATE_BPS),
}

/** Every copy, in the order they are added (so the light one keeps the media
 *  line it had before there was a middle one). */
internal val CAMERA_COPY_KINDS: List<CameraCopyKind> = listOf(CameraCopyKind.LIGHT, CameraCopyKind.MIDDLE)

/** The one encoding of the main camera sender. */
internal fun cameraMainEncoding(): RtpParameters.Encoding =
    RtpParameters.Encoding(null, true, null).apply {
        scalabilityMode = CAMERA_SCALABILITY_MODE
    }

/**
 * The one encoding of a copy's sender. No `rid`: this is an ordinary sender,
 * not a simulcast layer. [active] is stated only where the caller decides it
 * (the middle sender, which sends only from three people on); `null` leaves
 * the encoding active, as every encoding is by default.
 */
internal fun cameraCopyEncoding(kind: CameraCopyKind, active: Boolean? = null): RtpParameters.Encoding =
    RtpParameters.Encoding(null, active ?: true, kind.scaleDown).apply {
        maxBitrateBps = kind.maxBitrateBps
    }

/** One camera sender as this device means to send it. */
internal data class PlannedCameraSender(
    val kind: CameraSenderKind,
    /** The camera's picture scaled down by this, each side; 1 for the main one. */
    val scaleDown: Double,
    /** Bits a second; `null` for the main sender, which has no ceiling of its own. */
    val maxBitrateBps: Int?,
    val streamIndex: Int,
    /** Whether the sender's encoding is created active. */
    val active: Boolean,
)

/**
 * The camera senders of a call of [people] (this device's own person
 * included), in the order they are added: main, light, middle. All three
 * exist whatever the count, so no media line is added or removed when a third
 * person joins; the middle one is created inactive until [middleWanted] (the
 * shared `middle_sender_wanted` rule) says it is sent.
 */
internal fun cameraSenderPlan(people: Int, middleWanted: (people: Int) -> Boolean): List<PlannedCameraSender> {
    val middle = try {
        middleWanted(people)
    } catch (cause: RuntimeException) {
        // The rule could not be asked: the middle sender stays off.
        false
    } catch (cause: LinkageError) {
        // The native library that holds the rule is not loaded.
        false
    }
    return listOf(
        PlannedCameraSender(CameraSenderKind.MAIN, 1.0, null, SFRAME_STREAM_CAMERA, active = true),
        PlannedCameraSender(
            CameraSenderKind.LIGHT,
            CAMERA_LIGHT_SCALE_DOWN,
            CAMERA_LIGHT_MAX_BITRATE_BPS,
            SFRAME_STREAM_CAMERA_LIGHT,
            active = true,
        ),
        PlannedCameraSender(
            CameraSenderKind.MIDDLE,
            CAMERA_MIDDLE_SCALE_DOWN,
            CAMERA_MIDDLE_MAX_BITRATE_BPS,
            SFRAME_STREAM_CAMERA_MIDDLE,
            active = middle,
        ),
    )
}

/** How the main camera sender ended up being created, for the log line. */
internal class CameraSenderResult<S>(
    val sender: S,
    /** `true` when libwebrtc took the temporal-layer encoding with the
     *  transceiver; `false` when it refused and the track was added plainly. */
    val layered: Boolean,
)

/**
 * Add the camera as the main sender: one encoding in temporal layers on a
 * transceiver of its own. When libwebrtc refuses the mode the track is added
 * plainly and the layers are asked for afterwards ([requestCameraLayers]):
 * the call works either way, only less well under loss.
 *
 * libwebrtc 125's Java `addTransceiver` throws `IllegalStateException` when
 * the native call refuses (an encoding no negotiable codec supports is one
 * such refusal), so a throw from [addWithEncodings] is the refusal signal.
 *
 * @param addWithEncodings adds the track through a transceiver created with
 *   the given send encodings and returns its sender.
 * @param addPlain adds the track the way `addTrack` would.
 * @param onRefused told why the layered add failed, for the log line.
 */
internal fun <S> addCameraSender(
    addWithEncodings: (List<RtpParameters.Encoding>) -> S,
    addPlain: () -> S,
    onRefused: (cause: RuntimeException) -> Unit = {},
): CameraSenderResult<S> {
    try {
        return CameraSenderResult(addWithEncodings(listOf(cameraMainEncoding())), layered = true)
    } catch (cause: RuntimeException) {
        onRefused(cause)
    }
    return CameraSenderResult(addPlain(), layered = false)
}

/** What [addCopySender] did. */
internal class CopySenderAdded<S>(
    val sender: S,
    /**
     * `true` when libwebrtc took the encoding with the transceiver. `false`
     * when it refused and the transceiver was added plainly: the caller must
     * then [configureCopyEncoding] and remove the sender if that fails, or
     * the camera would be sent another time at full size.
     */
    val configured: Boolean,
)

/**
 * Add the [kind] copy of the camera. Returns `null` when libwebrtc will not
 * add another sender at all; [refused] is then told why. [active] is as in
 * [cameraCopyEncoding].
 *
 * @param addWithEncoding adds a send-only transceiver created with the given
 *   encoding and returns its sender.
 * @param addPlain adds a send-only transceiver with no encoding.
 */
internal fun <S> addCopySender(
    kind: CameraCopyKind,
    active: Boolean?,
    addWithEncoding: (RtpParameters.Encoding) -> S,
    addPlain: () -> S,
    refused: (what: String, cause: RuntimeException) -> Unit = { _, _ -> },
): CopySenderAdded<S>? {
    try {
        return CopySenderAdded(addWithEncoding(cameraCopyEncoding(kind, active)), configured = true)
    } catch (cause: RuntimeException) {
        // Tried again below without the encoding.
    }
    return try {
        CopySenderAdded(addPlain(), configured = false)
    } catch (cause: RuntimeException) {
        refused("a ${kind.sender.wireName} camera transceiver was refused", cause)
        null
    }
}

/**
 * Give an already-added copy's sender its size and its bitrate ceiling (the
 * second way, for a sender whose transceiver was added without them).
 * `false` when the platform refuses this too, or when the sender has no
 * encoding to set them on.
 *
 * @param encodings the encodings of the parameters the sender just reported;
 *   the first one is changed in place.
 * @param apply hands those parameters back to the sender and reports whether
 *   it accepted them (`RtpSender.setParameters`).
 */
internal fun configureCopyEncoding(
    encodings: List<RtpParameters.Encoding>,
    kind: CameraCopyKind,
    active: Boolean?,
    apply: () -> Boolean,
): Boolean {
    val first = encodings.firstOrNull() ?: return false
    first.scaleResolutionDownBy = kind.scaleDown
    first.maxBitrateBps = kind.maxBitrateBps
    if (active != null) first.active = active
    return try {
        apply()
    } catch (cause: RuntimeException) {
        false
    }
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
 * sender came from `addTrack`, which takes no encodings: the fallback of
 * [addCameraSender]. Best effort: a refusal keeps one layer and the call
 * goes on.
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

/**
 * The `mid`s an offer names for one copy of the camera: the one of that
 * copy's transceiver once it has one (after a local description that includes
 * it is set), none otherwise. Sent with every offer this client makes, so the
 * SFU forwards that media line as the camera's light companion, or as its
 * middle track.
 */
internal fun copyMids(mid: String?): List<String> =
    if (mid.isNullOrEmpty()) emptyList() else listOf(mid)

/**
 * The codec a sender's SFrame encryptor is told (ADR-0071): the name of the
 * first entry of the sender's negotiated codecs, or `null` before any codec
 * is negotiated. An encryptor told `null`, or a name with no rule, keeps
 * nothing of the frame in the clear.
 */
internal fun senderCodecName(negotiated: List<String?>): String? =
    negotiated.firstOrNull()?.takeIf { it.isNotEmpty() }
