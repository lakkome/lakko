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
 *
 * Built on the call's shared [LakkoCallKeys]; it opens through
 * their ring (current + previous key) and has no rekey of its
 * own (G-18, ADR-0029).
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
         * Build a fresh decryptor on the call's shared [keys]
         * (which it holds a native reference on). The decryptor takes NO stream argument
         * (ADR-0035): every open reads the composite KID `(sender_leaf
         * << 8) | stream_index` from the frame's own SFrame header and
         * derives that sender+stream's key internally, so one decryptor
         * bound to a remote receive track opens audio and every video
         * stream on it. Returns `null` on released keys or a
         * boot-probe negative. Per-(sender, stream)
         * ParticipantKey derivation is lazy — the first frame carrying a
         * new KID triggers one HKDF derivation; subsequent frames hit
         * the per-KID cache inside the C++ side.
         */
        @JvmStatic
        public fun create(
            keys: LakkoCallKeys,
        ): LakkoFrameDecryptor? {
            if (!SFrameAvailability.isCallingAvailable) return null
            val keysPtr = keys.pointer
            if (keysPtr == 0L) return null
            val ptr = nativeCreate(keysPtr)
            return if (ptr == 0L) null else LakkoFrameDecryptor(ptr)
        }

        @JvmStatic
        private external fun nativeCreate(keysPtr: Long): Long

        @JvmStatic
        private external fun nativeRelease(nativePtr: Long)
    }
}
