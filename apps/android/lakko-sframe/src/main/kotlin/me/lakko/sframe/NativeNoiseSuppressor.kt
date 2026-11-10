// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.sframe

import android.util.Log
import java.nio.ByteBuffer

/**
 * The capture noise suppressor's native half (ADR-0088): RNNoise from
 * `crates/lakko-noise`, through the `lakko_noise_*` C ABI of
 * `lakko-sframe-native`, reached by `noise_jni.cc` in `libsframe_jni.so`.
 *
 * A handle is one microphone stream's suppressor. [create] and [free]
 * allocate and free, so they run on a control thread; [process] allocates
 * nothing and takes no lock, and runs on libwebrtc's audio processing
 * thread. The caller must never [free] a handle while a [process] call may
 * hold it.
 */
public object NativeNoiseSuppressor {
    private const val TAG = "NativeNoiseSuppressor"

    /** [process] cleaned the samples. */
    public const val STATUS_OK: Int = 0

    /** [process] got no handle or no buffer; nothing was touched. */
    public const val STATUS_NULL: Int = 1

    /** [process] got more samples than one call takes; nothing was touched. */
    public const val STATUS_BLOCK_TOO_LONG: Int = 2

    /** [process] got a buffer that is not direct, too short, or misaligned;
     *  nothing was touched. */
    public const val STATUS_BAD_BUFFER: Int = 3

    /** [processAtRate] got a rate [supportsRate] refuses; nothing was
     *  touched. */
    public const val STATUS_UNSUPPORTED_RATE: Int = 4

    private val loaded: Boolean by lazy {
        try {
            System.loadLibrary("sframe_jni")
            true
        } catch (cause: UnsatisfiedLinkError) {
            Log.w(TAG, "libsframe_jni.so did not load: ${cause.message}")
            false
        }
    }

    /**
     * The sample rate the suppressor runs at (48 000), or `0` when the
     * native library or its noise functions are missing (a stale
     * `libsframe_jni.so`), in which case nothing else here may be called.
     */
    public val sampleRate: Int by lazy {
        if (!loaded) {
            0
        } else {
            try {
                nativeSampleRate()
            } catch (cause: UnsatisfiedLinkError) {
                Log.w(TAG, "libsframe_jni.so has no noise suppressor: ${cause.message}")
                0
            }
        }
    }

    /** A fresh suppressor, or `0` when it cannot be made. Control thread. */
    public fun create(): Long = try {
        if (sampleRate == 0) 0L else nativeCreate()
    } catch (cause: Throwable) {
        Log.w(TAG, "could not make a noise suppressor: ${cause.message}")
        0L
    }

    /** Frees [handle] made by [create]; `0` is a no-op. Control thread. */
    public fun free(handle: Long) {
        if (handle == 0L) return
        try {
            nativeFree(handle)
        } catch (cause: Throwable) {
            Log.w(TAG, "could not free a noise suppressor: ${cause.message}")
        }
    }

    /**
     * Cleans the first [frames] floats of the direct [buffer] in place
     * (16-bit PCM range, mono, at [sampleRate]). Returns a `STATUS_*`
     * value; nothing is touched unless it is [STATUS_OK]. Allocates
     * nothing; audio thread.
     */
    public fun process(handle: Long, buffer: ByteBuffer, frames: Int): Int =
        nativeProcess(handle, buffer, frames)

    /**
     * Whether [processAtRate] takes [rateHz]: 48 000, or a rate 48 000 is a
     * whole multiple of (24 000, 16 000, 12 000, 8 000). `false` when the
     * native library or the function is missing (a stale
     * `libsframe_jni.so`). Allocates nothing after the first call.
     */
    public fun supportsRate(rateHz: Int): Boolean = try {
        sampleRate != 0 && nativeSupportsRate(rateHz)
    } catch (cause: UnsatisfiedLinkError) {
        false
    }

    /**
     * [process] for a mono stream at [rateHz] (ADR-0088 amendment of
     * 2026-11-10): a rate below 48 kHz is raised to 48 kHz, cleaned and
     * lowered back in place. The rate may change between calls. Returns a
     * `STATUS_*` value ([STATUS_UNSUPPORTED_RATE] for a rate
     * [supportsRate] refuses); nothing is touched unless it is
     * [STATUS_OK]. Allocates nothing; audio thread.
     */
    public fun processAtRate(handle: Long, rateHz: Int, buffer: ByteBuffer, frames: Int): Int =
        nativeProcessAtRate(handle, rateHz, buffer, frames)

    private external fun nativeCreate(): Long

    private external fun nativeFree(handle: Long)

    private external fun nativeProcess(handle: Long, buffer: ByteBuffer, frames: Int): Int

    private external fun nativeSampleRate(): Int

    private external fun nativeSupportsRate(rateHz: Int): Boolean

    private external fun nativeProcessAtRate(handle: Long, rateHz: Int, buffer: ByteBuffer, frames: Int): Int
}
