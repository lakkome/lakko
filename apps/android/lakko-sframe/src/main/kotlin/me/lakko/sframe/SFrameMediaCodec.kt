// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.sframe

/**
 * The codec a sender negotiated, as far as the sealed-frame layout cares
 * (ADR-0071). It decides how many leading bytes of each encoded frame an
 * encryptor leaves in the clear so that a receiver without the generic
 * frame descriptor still finds key frames.
 *
 * [wire] is the matching `LAKKO_SFRAME_MEDIA_CODEC_*` constant of the
 * `lakko-sframe-native` C ABI.
 */
public enum class SFrameMediaCodec(public val wire: Int) {
    /**
     * Any codec without a rule (H.264, VP9, AV1, the RED / RTX / FEC
     * wrappers), and a codec that is not known yet. Nothing is left
     * clear. A fresh encryptor uses this until it is told otherwise.
     */
    OTHER(0),

    /** VP8 video: 10 clear bytes of a key frame, 3 of any other frame. */
    VP8(1),

    /** Opus audio. Nothing is left clear. */
    OPUS(2),
    ;

    public companion object {
        /** The codec for a C ABI value; an unknown value is [OTHER]. */
        @JvmStatic
        public fun fromWire(wire: Int): SFrameMediaCodec =
            entries.firstOrNull { it.wire == wire } ?: OTHER
    }
}
