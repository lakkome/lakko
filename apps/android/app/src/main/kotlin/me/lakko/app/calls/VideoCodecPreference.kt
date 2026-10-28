// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.webrtc.RtpCapabilities

/**
 * Negotiate VP8 for every call's video: the twin of the web
 * `video-codec-preference.ts`.
 *
 * A sealed media frame (ADR-0071) is opaque to libwebrtc's RTP packetizer,
 * which still packetizes by codec type after the frame encryptor ran. Only
 * VP8 has a rule for what stays in the clear. A packetizer that reads the
 * frame's own structure cannot carry a sealed frame: H264's looks for NAL
 * start codes, finds none and sends almost nothing. Measured in WebKit
 * (Playwright 1.63, two production web `CallSession`s): H264 negotiated, 113
 * video frames encoded, 0 packets sent; with VP8 forced, video crossed. In
 * Chromium with a transform scrambling every byte: VP8, 57 frames -> 194
 * packets; H264 forced, 51 frames -> 15 packets.
 *
 * Nothing chose the codec on this client: `DefaultVideoEncoderFactory`
 * happens to list VP8 first, but an offer from a peer whose factory lists
 * H264 first (iOS `RTCDefaultVideoEncoderFactory`, WebKit) could land on
 * H264 when this device answers. So before every local description every
 * video transceiver is told to prefer VP8 and nothing else that carries
 * media. The retransmission and redundancy formats stay, as they wrap the
 * VP8 payload rather than replace it. Not yet observed on a device.
 */
internal object VideoCodecPreference {
    /** The formats that wrap a media payload rather than carry one. */
    private val WRAPPERS = setOf("video/rtx", "video/red", "video/ulpfec", "video/flexfec-03")

    private const val VP8 = "video/vp8"

    /**
     * VP8 first, then the wrappers, each in the order [capabilities] lists
     * them; every other codec is dropped. `null` when no VP8 is offered.
     * Pure: [mimeOf] reads a capability's MIME type (`video/VP8`).
     */
    fun <T> vp8Preferences(capabilities: List<T>, mimeOf: (T) -> String?): List<T>? {
        val vp8 = capabilities.filter { mimeOf(it)?.lowercase() == VP8 }
        if (vp8.isEmpty()) return null
        val wrappers = capabilities.filter { mimeOf(it)?.lowercase() in WRAPPERS }
        return vp8 + wrappers
    }

    /** [vp8Preferences] over libwebrtc's receive capabilities. */
    fun vp8Preferences(
        capabilities: List<RtpCapabilities.CodecCapability>,
    ): List<RtpCapabilities.CodecCapability>? = vp8Preferences(capabilities, ::mimeTypeOf)

    /** The capability's MIME type; libwebrtc fills it as `video/<name>`. */
    private fun mimeTypeOf(capability: RtpCapabilities.CodecCapability): String? =
        capability.mimeType ?: capability.name?.let { "video/$it" }

    /** What [preferVp8] did. */
    data class Outcome(
        /** Video transceivers now preferring VP8. */
        val applied: Int,
        /** Video transceivers it could not set (no VP8 offered, or refused). */
        val unsupported: Int,
    )

    /**
     * Set [preferences] on every transceiver [isVideo] accepts; audio ones are
     * left alone. A `null` [preferences] (no VP8) or a refused set counts as
     * unsupported and leaves that transceiver as it is. Safe before every
     * description: the same list is set again.
     */
    fun <T, C> preferVp8(
        transceivers: List<T>,
        isVideo: (T) -> Boolean,
        preferences: List<C>?,
        set: (T, List<C>) -> Unit,
    ): Outcome {
        var applied = 0
        var unsupported = 0
        for (transceiver in transceivers) {
            if (!isVideo(transceiver)) continue
            if (preferences == null) {
                unsupported += 1
                continue
            }
            try {
                set(transceiver, preferences)
                applied += 1
            } catch (_: RuntimeException) {
                unsupported += 1
            }
        }
        return Outcome(applied, unsupported)
    }
}
