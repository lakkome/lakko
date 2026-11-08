// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.nio.ByteBuffer
import java.util.concurrent.Executor
import kotlin.random.Random
import me.lakko.sframe.NativeSegmenter
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.webrtc.CapturerObserver
import org.webrtc.VideoFrame

/**
 * The camera background stage (ADR-0089 section 3, `BackgroundStage.kt`)
 * wired into the capturer fan: while a background is selected, a frame the
 * background was not applied to never reaches a camera source (main,
 * middle, light, which is what [CapturerFan] feeds).
 *
 * The host JVM has no native library, so the segmenter is [FakeEngine],
 * which answers the frame gate by the table the Rust core defines
 * (`lakko_segment_frame_action`; the rule itself is tested in
 * `crates/lakko-segment`). What is tested here is the wiring: every path
 * through the stage, including every way the processing fails.
 */
class BackgroundStageTest {

    private companion object {
        fun cameraFrame(ts: Long) = VideoFrame(MarkedBuffer(processed = false), 270, ts)
    }

    /** A camera frame or a processed one, told apart by its buffer. */
    private class MarkedBuffer(val processed: Boolean) : VideoFrame.Buffer {
        var released = 0
        override fun getWidth(): Int = 1280
        override fun getHeight(): Int = 720
        override fun toI420(): VideoFrame.I420Buffer? = null
        override fun retain() = Unit
        override fun release() {
            released++
        }
        override fun cropAndScale(
            cropX: Int,
            cropY: Int,
            cropWidth: Int,
            cropHeight: Int,
            scaleWidth: Int,
            scaleHeight: Int,
        ): VideoFrame.Buffer = this
    }

    private class Recorder : CapturerObserver {
        val frames = mutableListOf<VideoFrame>()
        override fun onCapturerStarted(success: Boolean) = Unit
        override fun onCapturerStopped() = Unit
        override fun onFrameCaptured(frame: VideoFrame) {
            frames += frame
        }
        val rawCount: Int get() = frames.count { !(it.buffer as MarkedBuffer).processed }
    }

    /** The segmenter, answering the gate as the core does. */
    private class FakeEngine : SegmentEngine {
        var state = NativeSegmenter.STATE_WORKING
        var createResult = 7L
        var runThrows = false
        var gateOverride: Int? = null
        val restarts = mutableListOf<Long>()
        val freed = mutableListOf<Long>()
        override val available = true
        override val inputWidth = 256
        override val inputHeight = 144
        override fun create(nowMs: Long): Long = createResult
        override fun free(handle: Long) {
            freed += handle
        }
        override fun run(handle: Long, pixels: ByteBuffer, pixelsLen: Int, nowMs: Long, mask: ByteBuffer): Int {
            if (runThrows) throw IllegalStateException("inference failed")
            return state
        }
        override fun onFrame(handle: Long, nowMs: Long): Int = state
        override fun restart(handle: Long, nowMs: Long): Int {
            restarts += nowMs
            return state
        }
        override fun frameAction(selected: Boolean, state: Int): Int = gateOverride ?: when {
            !selected -> NativeSegmenter.GATE_SEND_CAMERA
            state == NativeSegmenter.STATE_STARTING -> NativeSegmenter.GATE_DROP
            state == NativeSegmenter.STATE_WORKING -> NativeSegmenter.GATE_SEND_PROCESSED
            else -> NativeSegmenter.GATE_DROP_AND_STOP_CAMERA
        }
    }

    private class FakeCompositor : BackgroundCompositor {
        var prepareThrows = false
        var compositeThrows = false
        var notReady = false
        var masks = 0
        var prepared = 0
        var composited = 0
        override fun prepareModelInput(frame: VideoFrame, out: ByteBuffer): Boolean {
            prepared++
            if (prepareThrows) throw IllegalStateException("GL error")
            return true
        }
        override fun uploadMask(mask: ByteBuffer) {
            masks++
        }
        override fun composite(frame: VideoFrame, look: BackgroundLook): CompositeResult {
            if (compositeThrows) throw IllegalStateException("GL error")
            if (notReady) return CompositeResult.NotReady
            composited++
            return CompositeResult.Frame(VideoFrame(MarkedBuffer(processed = true), frame.rotation, frame.timestampNs))
        }
        override fun release() = Unit
    }

    private class Events : BackgroundEvents {
        val statuses = mutableListOf<BackgroundStatus>()
        var failures = 0
        override fun status(status: BackgroundStatus) {
            statuses += status
        }
        override fun failed() {
            failures++
        }
    }

    private class Setup(
        selection: BackgroundSelection = BackgroundSelection.Blur(strong = false),
    ) {
        val engine = FakeEngine()
        val compositor = FakeCompositor()
        val events = Events()
        var selection: BackgroundSelection = selection
        var generation = 0L
        var now = 1_000L
        var paused = false
        val main = Recorder()
        val middle = Recorder()
        val light = Recorder()
        val stage = BackgroundStage(
            engine = engine,
            compositor = compositor,
            selection = { this.selection },
            generation = { generation },
            lookOf = { backgroundLookFor(it) { true } },
            // Inline: a model run finishes before the next frame.
            worker = Executor { it.run() },
            events = events,
            clock = { now },
            paused = { paused },
        )
        val fan = CapturerFan(main, stage).apply {
            attach(middle)
            attach(light)
            onCapturerStarted(true)
        }

        fun feed(count: Int) {
            repeat(count) {
                now += 33
                fan.onFrameCaptured(cameraFrame(now))
            }
        }

        val sources get() = listOf(main, middle, light)
        val rawSeen: Int get() = sources.sumOf { it.rawCount }
        val processedSeen: Int get() = sources.sumOf { it.frames.size - it.rawCount }
    }

    @Test
    fun `no background hands the camera on as it is`() {
        val s = Setup(BackgroundSelection.None)
        s.feed(3)
        assertEquals(9, s.rawSeen)
        assertEquals(0, s.processedSeen)
    }

    @Test
    fun `a working background hands on only processed frames, to every copy`() {
        val s = Setup()
        s.feed(5)
        assertEquals(0, s.rawSeen)
        // The first frame makes the segmenter (starting, dropped); the rest
        // are processed.
        s.sources.forEach { assertEquals(4, it.frames.size) }
        assertTrue(s.compositor.masks > 0)
    }

    @Test
    fun `a failing segmenter never lets a camera frame through and turns the camera off once`() {
        val s = Setup()
        s.engine.state = NativeSegmenter.STATE_FAILED
        s.feed(10)
        assertEquals(0, s.rawSeen)
        assertEquals(0, s.processedSeen)
        assertEquals(1, s.events.failures)
        assertEquals(BackgroundStatus.FAILED, s.events.statuses.last())
    }

    @Test
    fun `a segmenter that cannot be made fails closed`() {
        val s = Setup()
        s.engine.createResult = 0L
        s.feed(5)
        assertEquals(0, s.rawSeen + s.processedSeen)
        assertEquals(1, s.events.failures)
    }

    @Test
    fun `a compositor that throws fails closed`() {
        val s = Setup()
        s.compositor.compositeThrows = true
        s.feed(5)
        assertEquals(0, s.rawSeen + s.processedSeen)
        assertEquals(1, s.events.failures)
    }

    @Test
    fun `a model input that cannot be drawn fails closed`() {
        val s = Setup()
        s.compositor.prepareThrows = true
        s.feed(5)
        assertEquals(0, s.rawSeen + s.processedSeen)
        assertEquals(1, s.events.failures)
    }

    @Test
    fun `an inference that throws never lets a camera frame through`() {
        val s = Setup()
        s.engine.runThrows = true
        s.feed(5)
        assertEquals(0, s.rawSeen)
    }

    @Test
    fun `a starting segmenter drops every frame`() {
        val s = Setup()
        s.engine.state = NativeSegmenter.STATE_STARTING
        s.feed(5)
        assertEquals(0, s.rawSeen + s.processedSeen)
        assertEquals(0, s.events.failures)
        assertEquals(BackgroundStatus.STARTING, s.events.statuses.last())
    }

    @Test
    fun `a picture still loading drops the frame`() {
        val s = Setup(BackgroundSelection.Builtin("dawn"))
        s.compositor.notReady = true
        s.feed(5)
        assertEquals(0, s.rawSeen + s.processedSeen)
    }

    @Test
    fun `a gate answer of send camera while selected is not obeyed`() {
        val s = Setup()
        s.feed(1)
        s.engine.gateOverride = NativeSegmenter.GATE_SEND_CAMERA
        s.feed(5)
        assertEquals(0, s.rawSeen)
    }

    @Test
    fun `a restart after a failure lets processed frames through again and a new failure is reported`() {
        val s = Setup()
        s.feed(1)
        s.engine.state = NativeSegmenter.STATE_FAILED
        s.feed(3)
        assertEquals(1, s.events.failures)
        s.engine.state = NativeSegmenter.STATE_WORKING
        s.stage.requestRestart()
        s.feed(3)
        assertEquals(0, s.rawSeen)
        assertEquals(9, s.processedSeen)
        assertEquals(1, s.engine.restarts.size)
        s.engine.state = NativeSegmenter.STATE_FAILED
        s.feed(2)
        assertEquals(2, s.events.failures)
    }

    @Test
    fun `choosing a background again restarts the processing`() {
        val s = Setup()
        s.feed(2)
        s.generation += 1
        s.feed(1)
        assertEquals(1, s.engine.restarts.size)
    }

    @Test
    fun `a processed frame the fan made is released after the observers`() {
        val s = Setup()
        s.feed(3)
        val processed = s.main.frames.map { it.buffer as MarkedBuffer }
        assertTrue(processed.all { it.released == 1 })
    }

    @Test
    fun `under any mix of states and failures no camera frame reaches a source while selected`() {
        val random = Random(20261108)
        repeat(200) {
            val s = Setup()
            repeat(40) {
                s.engine.state = listOf(
                    NativeSegmenter.STATE_STARTING,
                    NativeSegmenter.STATE_WORKING,
                    NativeSegmenter.STATE_FAILED,
                    99,
                ).random(random)
                s.engine.runThrows = random.nextInt(10) == 0
                s.compositor.compositeThrows = random.nextInt(10) == 0
                s.compositor.prepareThrows = random.nextInt(10) == 0
                s.compositor.notReady = random.nextInt(5) == 0
                s.engine.gateOverride = if (random.nextInt(10) == 0) random.nextInt(-1, 6) else null
                if (random.nextInt(8) == 0) s.stage.requestRestart()
                if (random.nextInt(8) == 0) s.generation += 1
                s.feed(1)
            }
            assertEquals(0, s.rawSeen)
        }
    }

    @Test
    fun `with the camera off nothing is processed or sent, and coming back restarts the watch`() {
        val s = Setup()
        s.feed(3)
        val prepared = s.compositor.prepared
        val composited = s.compositor.composited
        val seen = s.processedSeen
        s.paused = true
        s.feed(10)
        assertEquals(prepared, s.compositor.prepared)
        assertEquals(composited, s.compositor.composited)
        assertEquals(seen, s.processedSeen)
        assertEquals(0, s.rawSeen)
        assertEquals(0, s.engine.restarts.size)
        s.paused = false
        s.feed(2)
        assertEquals(1, s.engine.restarts.size)
        assertEquals(0, s.rawSeen)
        assertTrue(s.processedSeen > seen)
    }

    @Test
    fun `a failure stays failed through a pause`() {
        val s = Setup()
        s.feed(1)
        s.engine.state = NativeSegmenter.STATE_FAILED
        s.feed(2)
        s.paused = true
        s.feed(3)
        s.paused = false
        s.engine.state = NativeSegmenter.STATE_WORKING
        s.feed(3)
        assertEquals(0, s.rawSeen + s.processedSeen)
        assertEquals(0, s.engine.restarts.size)
    }

    @Test
    fun `release frees the segmenter`() {
        val s = Setup()
        s.feed(2)
        s.stage.release()
        assertEquals(listOf(7L), s.engine.freed)
    }
}
