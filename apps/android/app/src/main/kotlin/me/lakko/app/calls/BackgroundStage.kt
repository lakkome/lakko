// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.nio.ByteBuffer
import java.util.concurrent.Executor
import java.util.concurrent.atomic.AtomicBoolean
import me.lakko.sframe.NativeSegmenter
import org.webrtc.VideoFrame

/**
 * A step between the camera and every camera sender ([CapturerFan]): given
 * one camera frame, the frame to hand on, or `null` to send nothing for it.
 * A returned frame other than the one given is the caller's to release.
 */
internal interface CameraFrameStage {
    fun process(frame: VideoFrame): VideoFrame?

    /** The capturer (re)started: a failed processing gets another chance. */
    fun onCapturerStarted()

    /** The camera was switched or turned back on: likewise. */
    fun requestRestart()

    /** Frees what the stage holds. On the capture thread, after its last
     *  frame. */
    fun release()
}

/** The segmenter as the stage drives it: [NativeSegmenter] on a device, a
 *  fake in the host tests. `STATE_*` and `GATE_*` are [NativeSegmenter]'s. */
internal interface SegmentEngine {
    val available: Boolean
    val inputWidth: Int
    val inputHeight: Int
    fun create(nowMs: Long): Long
    fun free(handle: Long)
    fun run(handle: Long, pixels: ByteBuffer, pixelsLen: Int, nowMs: Long, mask: ByteBuffer): Int
    fun onFrame(handle: Long, nowMs: Long): Int
    fun restart(handle: Long, nowMs: Long): Int

    /** The frame gate (`lakko_segment_frame_action`), the Rust core's rule. */
    fun frameAction(selected: Boolean, state: Int): Int
}

internal object NativeSegmentEngine : SegmentEngine {
    override val available: Boolean get() = NativeSegmenter.available
    override val inputWidth: Int get() = NativeSegmenter.inputWidth
    override val inputHeight: Int get() = NativeSegmenter.inputHeight
    override fun create(nowMs: Long): Long = NativeSegmenter.create(nowMs)
    override fun free(handle: Long) = NativeSegmenter.free(handle)
    override fun run(handle: Long, pixels: ByteBuffer, pixelsLen: Int, nowMs: Long, mask: ByteBuffer): Int =
        NativeSegmenter.run(handle, pixels, pixelsLen, NativeSegmenter.LAYOUT_RGBA, nowMs, mask)
    override fun onFrame(handle: Long, nowMs: Long): Int = NativeSegmenter.onFrame(handle, nowMs)
    override fun restart(handle: Long, nowMs: Long): Int = NativeSegmenter.restart(handle, nowMs)
    override fun frameAction(selected: Boolean, state: Int): Int = NativeSegmenter.frameAction(selected, state)
}

/** What [BackgroundCompositor.composite] made of a frame. */
internal sealed class CompositeResult {
    /** The frame with the background applied. */
    class Frame(val frame: VideoFrame) : CompositeResult()

    /** Nothing yet (a picture still loading, no mask, no free output):
     *  the frame is dropped. */
    object NotReady : CompositeResult()
}

/** The pixel work: [BackgroundGlCompositor] on a device. Throwing from any
 *  of these is a processing failure. Capture thread only. */
internal interface BackgroundCompositor {
    /** The frame upright in the model's input, RGBA rows top first, into
     *  [out]; `false` when it cannot be drawn. */
    fun prepareModelInput(frame: VideoFrame, out: ByteBuffer): Boolean
    fun uploadMask(mask: ByteBuffer)
    fun composite(frame: VideoFrame, look: BackgroundLook): CompositeResult
    fun release()
}

/** What the stage tells the rest of the app. */
internal interface BackgroundEvents {
    fun status(status: BackgroundStatus)

    /** The processing failed while a background is selected: turn the
     *  camera off and say so. Once per failure. */
    fun failed()
}

/** The processing state the call screen reads (UX section 3). */
internal enum class BackgroundStatus {
    /** No background selected, or no camera running. */
    OFF,

    /** Selected and getting ready: nothing is sent. */
    STARTING,

    /** Selected and applied. */
    WORKING,

    /** Selected and stopped: nothing is sent and the camera was turned off. */
    FAILED,
}

/**
 * The camera background stage (ADR-0089 sections 2 and 3): applies the
 * chosen background to each camera frame and asks the Rust core's frame
 * gate what may leave the device.
 *
 * The rule it carries out (UX section 3): **while a background is
 * selected, a frame the background was not applied to is never handed
 * on.** Every path that cannot produce a processed frame (the model still
 * loading, a picture still loading, no output texture free, an exception
 * anywhere, a gate answer this stage does not expect) drops the frame.
 *
 * Threads: [process], [onCapturerStarted] and [release] on the capture
 * thread; the model is made and run on [worker] (a single thread, so a run
 * and the free that follows it never overlap); [requestRestart] from any
 * thread.
 */
internal class BackgroundStage(
    private val engine: SegmentEngine,
    private val compositor: BackgroundCompositor,
    private val selection: () -> BackgroundSelection,
    private val generation: () -> Long,
    private val lookOf: (BackgroundSelection) -> BackgroundLook?,
    private val worker: Executor,
    private val events: BackgroundEvents,
    private val clock: () -> Long,
    /** Whether nothing would show a processed frame now (the camera is off
     *  and no preview is open): frames are then dropped without running the
     *  model or the compositor. */
    private val paused: () -> Boolean = { false },
) : CameraFrameStage {

    private val inputLen = engine.inputWidth * engine.inputHeight * 4
    private val maskLen = engine.inputWidth * engine.inputHeight
    private val input: ByteBuffer by lazy { ByteBuffer.allocateDirect(inputLen) }
    private val workMask: ByteBuffer by lazy { ByteBuffer.allocateDirect(maskLen) }
    private val readyMask: ByteBuffer by lazy { ByteBuffer.allocateDirect(maskLen) }
    private val maskLock = Any()
    private var maskReady = false

    @Volatile private var handle = 0L
    @Volatile private var creating = false
    @Volatile private var createFailed = false
    private val inFlight = AtomicBoolean(false)
    private val restartRequested = AtomicBoolean(false)
    @Volatile private var released = false

    /** A failure this stage saw itself (a compositor error); cleared by a
     *  restart, like the core's watch. */
    @Volatile private var failedHere = false
    private var failureReported = false
    private var lastGeneration = generation()
    private var lastStatus: BackgroundStatus? = null
    private var wasPaused = false

    override fun process(frame: VideoFrame): VideoFrame? = try {
        processOrThrow(frame)
    } catch (cause: Throwable) {
        // Never the camera frame: an error in the processing fails it.
        failedHere = true
        reportFailure()
        null
    }

    private fun processOrThrow(frame: VideoFrame): VideoFrame? {
        val chosen = selection()
        if (!chosen.isBackground) {
            publish(BackgroundStatus.OFF)
            if (!engine.available) return frame
            val action = engine.frameAction(false, NativeSegmenter.STATE_STARTING)
            return if (action == NativeSegmenter.GATE_SEND_CAMERA) frame else null
        }
        // The camera is off and no preview is open: nothing would show a
        // processed frame, so none is made and the model does not run. The
        // health watch is not fed either, and is restarted on the way back,
        // so a pause never reads as a stalled model.
        if (paused()) {
            wasPaused = true
            return null
        }
        if (wasPaused) {
            wasPaused = false
            // A real failure stays until the person acts (the camera on,
            // another choice); only the pause itself is forgiven.
            if (lastStatus != BackgroundStatus.FAILED) restartRequested.set(true)
        }
        val now = clock()
        val gen = generation()
        if (gen != lastGeneration) {
            lastGeneration = gen
            restartRequested.set(true)
        }
        val state = currentState(now)
        val action = if (engine.available) {
            engine.frameAction(true, state)
        } else {
            NativeSegmenter.GATE_DROP_AND_STOP_CAMERA
        }
        when (action) {
            NativeSegmenter.GATE_DROP_AND_STOP_CAMERA -> {
                // Final until a restart, as the core's watch is: latched here
                // too, so nothing but a restart lets frames through again.
                failedHere = true
                publish(BackgroundStatus.FAILED)
                reportFailure()
                return null
            }
            NativeSegmenter.GATE_DROP -> {
                publish(BackgroundStatus.STARTING)
                kickInference(frame, now)
                return null
            }
            NativeSegmenter.GATE_SEND_PROCESSED -> {
                kickInference(frame, now)
                uploadMaskIfReady()
                val look = lookOf(chosen) ?: BackgroundLook.Blur(strong = false)
                return when (val result = compositor.composite(frame, look)) {
                    is CompositeResult.Frame -> {
                        publish(BackgroundStatus.WORKING)
                        result.frame
                    }
                    CompositeResult.NotReady -> null
                }
            }
            // The core never answers "send the camera" while a background
            // is selected; whatever it is, the room is not shown.
            else -> return null
        }
    }

    /** The state to judge this frame by, starting or restarting the model
     *  as needed. */
    private fun currentState(now: Long): Int {
        val restart = restartRequested.getAndSet(false)
        if (restart) {
            failedHere = false
            failureReported = false
            synchronized(maskLock) { maskReady = false }
        }
        if (!engine.available || createFailed) return NativeSegmenter.STATE_FAILED
        val h = handle
        if (h == 0L) {
            if (!creating) {
                creating = true
                worker.execute {
                    val made = if (released) 0L else engine.create(clock())
                    if (released) {
                        engine.free(made)
                    } else if (made == 0L) {
                        createFailed = true
                    } else {
                        handle = made
                    }
                    creating = false
                }
            }
            return NativeSegmenter.STATE_STARTING
        }
        if (restart) engine.restart(h, now)
        if (failedHere) return NativeSegmenter.STATE_FAILED
        return engine.onFrame(h, now)
    }

    /** Hands the frame to the model when no run is in progress. */
    private fun kickInference(frame: VideoFrame, now: Long) {
        val h = handle
        if (h == 0L || !inFlight.compareAndSet(false, true)) return
        val prepared = try {
            compositor.prepareModelInput(frame, input)
        } catch (cause: Throwable) {
            inFlight.set(false)
            throw cause
        }
        if (!prepared) {
            inFlight.set(false)
            return
        }
        worker.execute {
            try {
                if (!released) {
                    engine.run(h, input, inputLen, clock(), workMask)
                    synchronized(maskLock) {
                        workMask.rewind()
                        readyMask.clear()
                        readyMask.put(workMask)
                        readyMask.rewind()
                        maskReady = true
                    }
                }
            } finally {
                inFlight.set(false)
            }
        }
    }

    private fun uploadMaskIfReady() {
        synchronized(maskLock) {
            if (!maskReady) return
            maskReady = false
            compositor.uploadMask(readyMask)
        }
    }

    private fun reportFailure() {
        if (failureReported) return
        failureReported = true
        events.failed()
    }

    private fun publish(status: BackgroundStatus) {
        if (status == lastStatus) return
        lastStatus = status
        events.status(status)
    }

    override fun onCapturerStarted() {
        restartRequested.set(true)
    }

    override fun requestRestart() {
        restartRequested.set(true)
    }

    override fun release() {
        released = true
        try {
            compositor.release()
        } catch (_: Throwable) {
            // Torn down with the camera's GL context either way.
        }
        if (lastStatus != null && lastStatus != BackgroundStatus.OFF) events.status(BackgroundStatus.OFF)
        worker.execute {
            val h = handle
            handle = 0L
            engine.free(h)
        }
    }
}
