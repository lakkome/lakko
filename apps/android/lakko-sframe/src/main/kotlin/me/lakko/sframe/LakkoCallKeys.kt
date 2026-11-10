// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.sframe

/**
 * The SFrame keying state of one call, shared by every
 * [LakkoFrameEncryptor] and [LakkoFrameDecryptor] of that call
 * (G-18, ADR-0029).
 *
 * Wraps the native `lakko::sframe::LakkoCallKeys`
 * (apps/shared/sframe-bridge/sframe-bridge.h): one call-key ring
 * (current + previous key) and a generation number. A mid-call
 * rekey is one [rekey] here and nothing else: every encryptor built
 * on these keys derives a fresh seal key on its next frame, every
 * decryptor opens through the shared ring. No transform has a rekey
 * of its own, so a platform has nothing to enumerate and cannot miss
 * one.
 *
 * Construct via [create]. Each transform built on the keys holds its
 * own native reference, so [release] may run while transforms are
 * still attached; the native keys go away with the last holder.
 */
public class LakkoCallKeys private constructor(
    private var nativePtr: Long,
) {

    /**
     * The native pointer the transforms are built on, or 0 once
     * [release] has run. Internal to the module: only
     * [LakkoFrameEncryptor.create] and [LakkoFrameDecryptor.create]
     * read it.
     */
    internal val pointer: Long
        get() = nativePtr

    /**
     * Rotate the whole call to the new MLS epoch's [seed] (32 bytes),
     * retaining the previous key until `nowMs + clamp(overlapMs)` so
     * in-flight frames sealed under the old epoch still open. Call
     * [prune] after the window to drop it (seed-boundary forward
     * secrecy).
     *
     * Returns `false` on a bad seed length, a negative time, or if
     * [release] already ran; nothing changes then, and the call keeps
     * sealing under the current key (never plaintext, security
     * invariant #4). Callable from any thread.
     */
    public fun rekey(seed: ByteArray, nowMs: Long, overlapMs: Long): Boolean {
        val ptr = nativePtr
        if (ptr == 0L) return false
        if (seed.size != SEED_LEN) return false
        if (nowMs < 0 || overlapMs < 0) return false
        return nativeRekey(ptr, seed, nowMs, overlapMs)
    }

    /**
     * Drop the retained previous key once the overlap window has
     * elapsed at [nowMs] (G-18, ADR-0029). A no-op once [release] has
     * run.
     */
    public fun prune(nowMs: Long) {
        val ptr = nativePtr
        if (ptr == 0L) return
        nativePrune(ptr, nowMs)
    }

    /**
     * Drop this object's native reference. Idempotent; calling twice
     * is a no-op on the second call. Transforms built on the keys keep
     * them alive until they are released themselves.
     */
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

        /** The length of a per-call seed. */
        public const val SEED_LEN: Int = 32

        /**
         * Build the call keys from the per-call MLS exporter seed (32
         * bytes, `Group.export_secret("lakko/call/v1", call_id, 32)`).
         * Returns `null` on a bad seed length, a native refusal, or if
         * [SFrameAvailability] reports calling is unavailable on this
         * build; a call without keys arms nothing.
         */
        @JvmStatic
        public fun create(seed: ByteArray): LakkoCallKeys? {
            if (!SFrameAvailability.isCallingAvailable) return null
            if (seed.size != SEED_LEN) return null
            val ptr = nativeCreate(seed)
            return if (ptr == 0L) null else LakkoCallKeys(ptr)
        }

        @JvmStatic
        private external fun nativeCreate(seed: ByteArray): Long

        @JvmStatic
        private external fun nativeRekey(
            nativePtr: Long,
            seed: ByteArray,
            nowMs: Long,
            overlapMs: Long,
        ): Boolean

        @JvmStatic
        private external fun nativePrune(nativePtr: Long, nowMs: Long)

        @JvmStatic
        private external fun nativeRelease(nativePtr: Long)
    }
}
