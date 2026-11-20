// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.sframe

import android.util.Log
import java.nio.ByteBuffer

/**
 * The camera background segmenter's native half (ADR-0089): the selfie
 * segmentation model, its mask smoothing, its health watch and the frame
 * gate from `crates/lakko-segment`, through the `lakko_segment_*` C ABI of
 * `lakko-sframe-native`, reached by `segment_jni.cc` in `libsframe_jni.so`.
 *
 * A handle is one camera's segmenter. [create] loads the model (tens of
 * milliseconds) and [free] releases it: both off the capture thread.
 * [run] runs on an inference worker; [onFrame] and [state] on the capture
 * thread, never waiting for an inference. The caller must never [free] a
 * handle while another call may hold it.
 */
public object NativeSegmenter {
    private const val TAG = "NativeSegmenter"

    /** [state] values (`lakko_segment_state_t`). */
    public const val STATE_STARTING: Int = 0
    public const val STATE_WORKING: Int = 1
    public const val STATE_FAILED: Int = 2

    /** [frameAction] values (`lakko_frame_action_t`). */
    public const val GATE_SEND_CAMERA: Int = 0
    public const val GATE_SEND_PROCESSED: Int = 1
    public const val GATE_DROP: Int = 2
    public const val GATE_DROP_AND_STOP_CAMERA: Int = 3

    /** [run] layouts (`lakko_segment_layout_t`). */
    public const val LAYOUT_RGBA: Int = 0

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
     * `true` when the native library carries the segmenter. `false` for a
     * stale `libsframe_jni.so`, in which case nothing else here may be
     * called and the background section is absent.
     */
    public val available: Boolean by lazy {
        if (!loaded) {
            false
        } else {
            try {
                nativeInputWidth() > 0
            } catch (cause: UnsatisfiedLinkError) {
                Log.w(TAG, "libsframe_jni.so has no segmenter: ${cause.message}")
                false
            }
        }
    }

    /** Width of the model's input picture and of the mask (256). */
    public val inputWidth: Int by lazy { if (available) nativeInputWidth() else 256 }

    /** Height of the model's input picture and of the mask (144). */
    public val inputHeight: Int by lazy { if (available) nativeInputHeight() else 144 }

    /** A new segmenter, or `0` when the native side is missing. Never on
     *  the capture thread. */
    public fun create(nowMs: Long): Long = try {
        if (!available) 0L else nativeNew(nowMs)
    } catch (cause: Throwable) {
        Log.w(TAG, "could not make a segmenter: ${cause.javaClass.simpleName}")
        0L
    }

    /** Frees [handle]; `0` is a no-op. */
    public fun free(handle: Long) {
        if (handle == 0L) return
        try {
            nativeFree(handle)
        } catch (cause: Throwable) {
            Log.w(TAG, "could not free a segmenter: ${cause.javaClass.simpleName}")
        }
    }

    /**
     * Segments the first [pixelsLen] bytes of the direct [pixels]
     * ([inputWidth] x [inputHeight] in [layout]) into the direct [mask].
     * Returns a `STATE_*` value; on failure [mask] is left as it was.
     */
    public fun run(handle: Long, pixels: ByteBuffer, pixelsLen: Int, layout: Int, nowMs: Long, mask: ByteBuffer): Int =
        nativeRun(handle, pixels, pixelsLen, layout, nowMs, mask)

    /** A camera frame arrived: the state to judge it by. */
    public fun onFrame(handle: Long, nowMs: Long): Int = nativeOnFrame(handle, nowMs)

    /** Restarts the health watch (a background chosen again, the camera
     *  switched or turned back on). */
    public fun restart(handle: Long, nowMs: Long): Int = nativeRestart(handle, nowMs)

    /** The current state, without judging a frame. */
    public fun state(handle: Long): Int = nativeState(handle)

    /** The frame gate (`GATE_*`). */
    public fun frameAction(selected: Boolean, state: Int): Int = nativeFrameAction(selected, state)

    private external fun nativeNew(nowMs: Long): Long

    private external fun nativeFree(handle: Long)

    private external fun nativeRun(
        handle: Long,
        pixels: ByteBuffer,
        pixelsLen: Int,
        layout: Int,
        nowMs: Long,
        mask: ByteBuffer,
    ): Int

    private external fun nativeOnFrame(handle: Long, nowMs: Long): Int

    private external fun nativeRestart(handle: Long, nowMs: Long): Int

    private external fun nativeState(handle: Long): Int

    private external fun nativeFrameAction(selected: Boolean, state: Int): Int

    private external fun nativeInputWidth(): Int

    private external fun nativeInputHeight(): Int
}
