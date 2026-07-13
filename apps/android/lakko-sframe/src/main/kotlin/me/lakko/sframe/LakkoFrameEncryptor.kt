// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.sframe

import org.webrtc.FrameEncryptor

/**
 * SFrame encryptor for outgoing RTP frames on a single
 * RTCRtpSender (G3-sframe-native N-5).
 *
 * Implements `org.webrtc.FrameEncryptor` — libwebrtc's Java-
 * side glue reads `getNativeFrameEncryptor()` and hands the
 * `long` (raw `void*`) to the C++ side, which wraps it in
 * `rtc::scoped_refptr<webrtc::FrameEncryptorInterface>`.
 * The pointer is owned by a `LakkoFrameEncryptor` C++ object
 * (apps/shared/sframe-bridge/sframe-bridge.cc N-3) that
 * trampolines through the lakko-sframe-native C ABI (N-1) for
 * every AEAD seal + RFC 9605 wire-header pack.
 *
 * Construct via [LakkoFrameEncryptor.create] — the constructor
 * is private so the only path into the class goes through the
 * boot-probe gate inside [create]. Returns `null` on bad seed
 * length, HKDF failure, or if [SFrameAvailability] reports
 * calling is unavailable on this build.
 *
 * The native pointer is freed by [release]; the caller MUST
 * call it once after detaching the encryptor from the sender.
 * libwebrtc's Java-side glue calls `getNativeFrameEncryptor()`
 * multiple times during the lifetime of an attached sender;
 * releasing the pointer while the sender is still attached
 * causes a use-after-free on the codec thread.
 */
public class LakkoFrameEncryptor private constructor(
    @get:JvmName("getNativeFrameEncryptorPointer")
    private var nativePtr: Long,
) : FrameEncryptor {

    /**
     * libwebrtc's standard Java surface — returns the raw
     * native pointer the C++ side casts to
     * `webrtc::FrameEncryptorInterface*`.
     *
     * Returns 0 (libwebrtc's sentinel for "no encryptor") if
     * [release] has already been called.
     */
    override fun getNativeFrameEncryptor(): Long = nativePtr

    /**
     * Drop the native pointer. Idempotent; calling twice is a
     * no-op on the second call. Must run AFTER the
     * RTCRtpSender has been told to detach via
     * `sender.setFrameEncryptor(null)` — otherwise libwebrtc's
     * codec thread can still hold a live reference to the
     * pointer we're about to free.
     */
    public fun release() {
        val ptr = nativePtr
        if (ptr != 0L) {
            nativePtr = 0L
            nativeRelease(ptr)
        }
    }

    /**
     * Mid-call rekey (G-18, ADR-0029): swap the seal key to the new MLS
     * epoch's [seed], keeping the same local leaf. The encryptor seals
     * only under the current epoch, so this is a straight key swap —
     * already-sealed frames are already bytes in libwebrtc's pipeline and
     * need no overlap.
     *
     * Returns `false` on a bad seed length (must be 32), an HKDF failure,
     * or if [release] already ran — in which case the encryptor keeps
     * sealing under the current key (never plaintext, security invariant
     * #4). Thread-safe against a concurrent seal on the codec thread.
     */
    public fun rekey(seed: ByteArray): Boolean {
        val ptr = nativePtr
        if (ptr == 0L) return false
        if (seed.size != 32) return false
        return nativeRekey(ptr, seed)
    }

    @Suppress("ProtectedInFinal")
    protected fun finalize() {
        // Safety net for the case where the consumer forgot to
        // call release(). Native release is idempotent and
        // refcount-safe per N-3's documentation; this finalize
        // is just a defensive cleanup.
        release()
    }

    public companion object {
        init {
            // System.loadLibrary is idempotent — calling it
            // here and again from LakkoFrameDecryptor's
            // companion init both work; the runtime no-ops the
            // second invocation. Naming `sframe_jni` matches
            // `libsframe_jni.so` shipped under
            // `src/main/jniLibs/{abi}/`.
            System.loadLibrary("sframe_jni")
        }

        /**
         * Build a fresh encryptor for the per-call MLS exporter
         * seed (32 bytes — `Group.export_secret("lakko/call/v1",
         * call_id, 32)`) + this device's leaf index. Returns
         * `null` on any failure (bad seed length, HKDF failure,
         * boot-probe negative).
         */
        @JvmStatic
        public fun create(seed: ByteArray, localLeaf: Int): LakkoFrameEncryptor? {
            if (!SFrameAvailability.isCallingAvailable) return null
            if (seed.size != 32) return null
            val ptr = nativeCreate(seed, localLeaf)
            return if (ptr == 0L) null else LakkoFrameEncryptor(ptr)
        }

        @JvmStatic
        private external fun nativeCreate(seed: ByteArray, localLeaf: Int): Long

        @JvmStatic
        private external fun nativeRelease(nativePtr: Long)

        @JvmStatic
        private external fun nativeRekey(nativePtr: Long, seed: ByteArray): Boolean
    }
}
