// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Test
import org.webrtc.CapturerObserver
import org.webrtc.VideoFrame

/**
 * The capturer fan (`CapturerFan.kt`): every attached source gets every
 * frame and the capturer's state, a source attached while the camera runs is
 * told it started, and the main source is never detached. `VideoFrame` is a
 * plain Java class; the buffer here is a stand-in no native code touches.
 */
class CapturerFanTest {

    private class Recorder : CapturerObserver {
        val events = mutableListOf<String>()
        override fun onCapturerStarted(success: Boolean) {
            events += "started=$success"
        }
        override fun onCapturerStopped() {
            events += "stopped"
        }
        override fun onFrameCaptured(frame: VideoFrame) {
            events += "frame=${frame.timestampNs}"
        }
    }

    private object StandInBuffer : VideoFrame.Buffer {
        override fun getWidth(): Int = 1280
        override fun getHeight(): Int = 720
        override fun toI420(): VideoFrame.I420Buffer? = null
        override fun retain() = Unit
        override fun release() = Unit
        override fun cropAndScale(
            cropX: Int,
            cropY: Int,
            cropWidth: Int,
            cropHeight: Int,
            scaleWidth: Int,
            scaleHeight: Int,
        ): VideoFrame.Buffer = this
    }

    private fun frame(ts: Long) = VideoFrame(StandInBuffer, 0, ts)

    @Test
    fun `every attached source gets every frame`() {
        val main = Recorder()
        val light = Recorder()
        val middle = Recorder()
        val fan = CapturerFan(main)
        fan.attach(light)
        fan.attach(middle)
        fan.onCapturerStarted(true)
        fan.onFrameCaptured(frame(1))
        fan.onFrameCaptured(frame(2))
        val expected = listOf("started=true", "frame=1", "frame=2")
        assertEquals(expected, main.events)
        assertEquals(expected, light.events)
        assertEquals(expected, middle.events)
    }

    @Test
    fun `a source attached while the camera runs is told it started`() {
        val main = Recorder()
        val late = Recorder()
        val fan = CapturerFan(main)
        fan.onCapturerStarted(true)
        fan.onFrameCaptured(frame(1))
        fan.attach(late)
        fan.onFrameCaptured(frame(2))
        assertEquals(listOf("started=true", "frame=2"), late.events)
    }

    @Test
    fun `a source attached before the start or after the stop is told nothing yet`() {
        val fan = CapturerFan(Recorder())
        val early = Recorder()
        fan.attach(early)
        assertEquals(emptyList<String>(), early.events)
        fan.onCapturerStarted(true)
        fan.onCapturerStopped()
        val afterStop = Recorder()
        fan.attach(afterStop)
        assertEquals(emptyList<String>(), afterStop.events)
        assertEquals(listOf("started=true", "stopped"), early.events)
    }

    @Test
    fun `attaching twice hands each frame once`() {
        val main = Recorder()
        val copy = Recorder()
        val fan = CapturerFan(main)
        fan.attach(copy)
        fan.attach(copy)
        fan.onFrameCaptured(frame(7))
        assertEquals(2, fan.size)
        assertEquals(listOf("frame=7"), copy.events)
    }

    @Test
    fun `a detached copy gets no more frames and the main source cannot be detached`() {
        val main = Recorder()
        val copy = Recorder()
        val fan = CapturerFan(main)
        fan.attach(copy)
        fan.detach(copy)
        fan.detach(main)
        fan.onFrameCaptured(frame(3))
        assertEquals(listOf("frame=3"), main.events)
        assertEquals(emptyList<String>(), copy.events)
        assertEquals(1, fan.size)
    }
}
