// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.sframe

import org.webrtc.FrameDecryptor

/**
 * SFrame decryptor for incoming RTP frames on a single
 * RTCRtpReceiver (G3-sframe-native N-5).
 *
 * Mirror of [LakkoFrameEncryptor] — same C++-pointer-via-Long
 * pattern. The underlying `LakkoFrameDecryptor` C++ object
 * (apps/shared/sframe-bridge/sframe-bridge.cc) lazily derives
 * + caches one `ParticipantKey` per remote `sender_leaf`
 * observed in the SFrame wire header, so the same decryptor
 * can fan in N senders on one receiver — matches how the SFU
 * forwards frames in a multi-party call.
 */
public class LakkoFrameDecryptor private constructor(
    @get:JvmName("getNativeFrameDecryptorPointer")
    private var nativePtr: Long,
) : FrameDecryptor {

    override fun getNativeFrameDecryptor(): Long = nativePtr

    public fun release() {
        val ptr = nativePtr
        if (ptr != 0L) {
            nativePtr = 0L
            nativeRelease(ptr)
        }
    }

    @Suppress("ProtectedInFinal")
    protected fun finalize() {
        release()
    }

    public companion object {
        init {
            System.loadLibrary("sframe_jni")
        }

        /**
         * Build a fresh decryptor for the per-call MLS exporter
         * seed (32 bytes). Same constraints as [LakkoFrame
         * Encryptor.create]. Per-sender ParticipantKey
         * derivation is lazy — the first frame from each new
         * `sender_leaf` triggers one HKDF derivation; subsequent
         * frames hit the per-sender cache inside the C++ side.
         */
        @JvmStatic
        public fun create(seed: ByteArray): LakkoFrameDecryptor? {
            if (!SFrameAvailability.isCallingAvailable) return null
            if (seed.size != 32) return null
            val ptr = nativeCreate(seed)
            return if (ptr == 0L) null else LakkoFrameDecryptor(ptr)
        }

        @JvmStatic
        private external fun nativeCreate(seed: ByteArray): Long

        @JvmStatic
        private external fun nativeRelease(nativePtr: Long)
    }
}
