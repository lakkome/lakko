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

    /**
     * Mid-call rekey (G-18, ADR-0029): rotate the call-key ring to the new
     * MLS epoch's [seed], retaining the previous epoch's key until `nowMs +
     * clamp(overlapMs)` so in-flight frames sealed under the old epoch
     * still open. Call [prune] after the window to drop it (seed-boundary
     * forward secrecy).
     *
     * Returns `false` on a bad seed length (must be 32) or if [release]
     * already ran — in which case the ring keeps its current + previous
     * keys (never plaintext, security invariant #4). Thread-safe (the Rust
     * ring is Mutex-guarded internally).
     */
    public fun rekey(seed: ByteArray, nowMs: Long, overlapMs: Long): Boolean {
        val ptr = nativePtr
        if (ptr == 0L) return false
        if (seed.size != 32) return false
        return nativeRekey(ptr, seed, nowMs, overlapMs)
    }

    /**
     * Drop the retained previous key once the overlap window has elapsed at
     * [nowMs] (G-18, ADR-0029) — closes the window for seed-boundary
     * forward secrecy. A no-op once [release] has run.
     */
    public fun prune(nowMs: Long) {
        val ptr = nativePtr
        if (ptr == 0L) return
        nativePrune(ptr, nowMs)
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

        @JvmStatic
        private external fun nativeRekey(
            nativePtr: Long,
            seed: ByteArray,
            nowMs: Long,
            overlapMs: Long,
        ): Boolean

        @JvmStatic
        private external fun nativePrune(nativePtr: Long, nowMs: Long)
    }
}
