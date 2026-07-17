// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.sframe

/**
 * The SFrame per-participant key media-type space (ADR-0034).
 *
 * A single participant seals two independent RTP streams — audio and video —
 * from the same leaf index. Without a media-type split both streams would
 * derive the SAME `(key, nonce_salt)` and, each carrying its own independent
 * per-stream counter, reuse the identical `(key, nonce)` pair across the two
 * streams on different plaintexts (a catastrophic AEAD nonce reuse). ADR-0034
 * folds the media type into the HKDF `info` so audio and video derive on
 * distinct key + salt spaces.
 *
 * An encryptor / decryptor is bound to exactly one media type for its
 * lifetime: the local send track's kind for an encryptor, the remote receive
 * track's kind for a decryptor. A frame sealed under one media type only opens
 * under a decryptor armed on the same media type (fail-closed).
 *
 * [abiValue] mirrors the `lakko-sframe-native` C ABI constants
 * `LAKKO_SFRAME_MEDIA_TYPE_AUDIO` (1) / `LAKKO_SFRAME_MEDIA_TYPE_VIDEO` (2).
 * The JNI shim (`sframe_jni.cc`) passes the raw int through to the C++ bridge
 * `LakkoFrameEncryptor::Create` / `LakkoFrameDecryptor::Create`, which refuse
 * an unknown value. This enum is deliberately standalone (not the UniFFI
 * `FfiMediaType`) because the `lakko-sframe` module does not depend on
 * `lakko-core`, and the per-frame path goes through the JNI C ABI, not UniFFI.
 */
public enum class SFrameMediaType(
    /** The `lakko-sframe-native` C ABI value (1 = audio, 2 = video). */
    public val abiValue: Int,
) {
    /** Audio stream. */
    AUDIO(1),

    /** Video stream (the camera track). */
    VIDEO(2),
}
