// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The capture post-processing stage of ADR-0088 against a fake native
 * suppressor (the host JVM has no native library): when it cleans, when it
 * leaves the buffer alone, and that a suppressor is never freed under the
 * audio thread.
 */
class CaptureNoiseProcessorTest {

    private class FakeEngine(override val sampleRate: Int = 48_000) : NoiseEngine {
        var next = 1L
        val created = mutableListOf<Long>()
        val freed = mutableListOf<Long>()
        val processed = mutableListOf<Long>()
        val rates = mutableListOf<Int>()
        val supported = setOf(48_000, 24_000, 16_000, 12_000, 8_000)
        var status = 0
        var throwOnProcess = false
        var createFails = false
        var gate: CountDownLatch? = null
        var entered: CountDownLatch? = null

        override fun create(): Long {
            if (createFails) return 0L
            val h = next++
            created += h
            return h
        }

        override fun free(handle: Long) {
            synchronized(this) { freed += handle }
        }

        override fun supportsRate(rateHz: Int): Boolean = rateHz in supported

        override fun processAtRate(handle: Long, rateHz: Int, buffer: ByteBuffer, frames: Int): Int {
            entered?.countDown()
            gate?.await(5, TimeUnit.SECONDS)
            if (throwOnProcess) throw IllegalStateException("boom")
            synchronized(this) {
                processed += handle
                rates += rateHz
            }
            if (status == 0) buffer.putFloat(0, 0f)
            return status
        }
    }

    private fun buffer(frames: Int = 480): ByteBuffer =
        ByteBuffer.allocateDirect(frames * 4).order(ByteOrder.nativeOrder()).apply {
            for (i in 0 until frames) putFloat(i * 4, 1000f)
        }

    private fun processor(engine: FakeEngine, lines: MutableList<String>) =
        CaptureNoiseProcessor(engine = engine, log = { lines += it }, pause = { Thread.sleep(1) })

    @Test
    fun cleansAt48kMonoWhenOn() {
        val engine = FakeEngine()
        val lines = mutableListOf<String>()
        val p = processor(engine, lines)
        p.initialize(48_000, 1)
        p.arm(owner = this, on = true, platform = "platform test")
        val buf = buffer()
        p.process(3, 480, buf)
        assertEquals(listOf(1L), engine.processed)
        assertEquals(0f, buf.getFloat(0))
        assertTrue(p.running)
        assertTrue(lines.any { it.contains("RNNoise noise suppression on") && it.contains("48000 Hz mono") })
    }

    @Test
    fun offLeavesTheBufferAlone() {
        val engine = FakeEngine()
        val lines = mutableListOf<String>()
        val p = processor(engine, lines)
        p.initialize(48_000, 1)
        p.arm(owner = this, on = false, platform = "x")
        val buf = buffer()
        p.process(3, 480, buf)
        assertTrue(engine.created.isEmpty())
        assertTrue(engine.processed.isEmpty())
        assertEquals(1000f, buf.getFloat(0))
        assertTrue(lines.any { it.contains("noise suppression off") })
    }

    @Test
    fun sixteenKiloHertzMonoIsCleanedThroughTheRateCall() {
        val engine = FakeEngine()
        val lines = mutableListOf<String>()
        val p = processor(engine, lines)
        p.initialize(16_000, 1)
        p.arm(owner = this, on = true, platform = "x")
        val buf = buffer(160)
        p.process(1, 160, buf)
        assertEquals(listOf(1L), engine.processed)
        assertEquals(listOf(16_000), engine.rates)
        assertEquals(0f, buf.getFloat(0))
        assertTrue(lines.any { it.contains("capture 16000 Hz mono, raised to 48000 Hz for RNNoise") })
        assertTrue(lines.none { it.contains("bypassed") })
    }

    @Test
    fun fortyFourPointOneKiloHertzIsPassedThrough() {
        val engine = FakeEngine()
        val lines = mutableListOf<String>()
        val p = processor(engine, lines)
        p.initialize(44_100, 1)
        p.arm(owner = this, on = true, platform = "x")
        val buf = buffer(441)
        p.process(1, 441, buf)
        assertTrue(engine.processed.isEmpty())
        assertEquals(1000f, buf.getFloat(0))
        assertTrue(lines.any { it.contains("44100 Hz") && it.contains("bypassed") })
    }

    @Test
    fun aBlockThatIsNotTenMillisecondsIsPassedThrough() {
        val engine = FakeEngine()
        val p = processor(engine, mutableListOf())
        p.initialize(16_000, 1)
        p.arm(owner = this, on = true, platform = "x")
        p.process(1, 480, buffer())
        assertTrue(engine.processed.isEmpty())
    }

    @Test
    fun aFormatChangeFromSixteenToFortyEightMidCallKeepsCleaning() {
        val engine = FakeEngine()
        val lines = mutableListOf<String>()
        val p = processor(engine, lines)
        p.initialize(16_000, 1)
        p.arm(owner = this, on = true, platform = "x")
        p.process(1, 160, buffer(160))
        p.reset(48_000)
        p.process(3, 480, buffer())
        // One suppressor for the whole call; the rate goes with each block.
        assertEquals(listOf(1L, 1L), engine.processed)
        assertEquals(listOf(16_000, 48_000), engine.rates)
        assertTrue(engine.freed.isEmpty())
        assertTrue(lines.any { it.contains("format 16000 Hz, 1 ch") && it.contains("raised to 48000 Hz") })
        assertTrue(lines.any { it.contains("format 48000 Hz, 1 ch") && it.contains("RNNoise applies") })
        // A stage that then leaves for an unsupported rate passes through.
        p.reset(32_000)
        p.process(2, 320, buffer(320))
        assertEquals(2, engine.processed.size)
        assertTrue(lines.any { it.contains("32000 Hz") && it.contains("bypassed") })
    }

    @Test
    fun stereoIsPassedThrough() {
        val engine = FakeEngine()
        val lines = mutableListOf<String>()
        val p = processor(engine, lines)
        p.initialize(48_000, 2)
        p.arm(owner = this, on = true, platform = "x")
        p.process(3, 480, buffer())
        assertTrue(engine.processed.isEmpty())
        assertTrue(lines.any { it.contains("2 ch") && it.contains("bypassed") && it.contains("mono") })
    }

    @Test
    fun switchAppliesAtOnceWithAFreshSuppressor() {
        val engine = FakeEngine()
        val lines = mutableListOf<String>()
        val p = processor(engine, lines)
        p.initialize(48_000, 1)
        p.arm(owner = this, on = true, platform = "x")
        p.process(3, 480, buffer())
        p.setEnabled(false)
        assertEquals(listOf(1L), engine.freed)
        p.process(3, 480, buffer())
        assertEquals(listOf(1L), engine.processed)
        p.setEnabled(true)
        p.process(3, 480, buffer())
        // Never the old one again: no audio of the earlier stretch leaks in.
        assertEquals(listOf(1L, 2L), engine.processed)
        assertTrue(lines.any { it.contains("switched off") })
        assertTrue(lines.any { it.contains("switched on") })
    }

    @Test
    fun switchWithNoCallOnlyAppliesToTheNext() {
        val engine = FakeEngine()
        val p = processor(engine, mutableListOf())
        p.setEnabled(true)
        assertTrue(engine.created.isEmpty())
    }

    @Test
    fun aRefusedBufferStopsTheSuppressorForTheCallAndLogsOnce() {
        val engine = FakeEngine().apply { status = 3 }
        val lines = mutableListOf<String>()
        val p = processor(engine, lines)
        p.initialize(48_000, 1)
        p.arm(owner = this, on = true, platform = "x")
        val buf = buffer()
        p.process(3, 480, buf)
        p.process(3, 480, buffer())
        assertEquals(1, engine.processed.size)
        assertEquals(1000f, buf.getFloat(0))
        assertEquals(1, lines.count { it.contains("failed") })
        assertFalse(p.running)
    }

    @Test
    fun aThrowNeverReachesLibwebrtc() {
        val engine = FakeEngine().apply { throwOnProcess = true }
        val lines = mutableListOf<String>()
        val p = processor(engine, lines)
        p.initialize(48_000, 1)
        p.arm(owner = this, on = true, platform = "x")
        p.process(3, 480, buffer())
        p.process(3, 480, buffer())
        assertEquals(1, lines.count { it.contains("failed") })
    }

    @Test
    fun noNativeSuppressorMeansPlatformOnly() {
        val engine = FakeEngine().apply { createFails = true }
        val lines = mutableListOf<String>()
        val p = processor(engine, lines)
        p.initialize(48_000, 1)
        p.arm(owner = this, on = true, platform = "x")
        p.process(3, 480, buffer())
        assertTrue(engine.processed.isEmpty())
        assertTrue(lines.any { it.contains("unavailable") })
    }

    @Test
    fun aStaleDisarmLeavesTheNewCallAlone() {
        val engine = FakeEngine()
        val p = processor(engine, mutableListOf())
        p.initialize(48_000, 1)
        val oldCall = Any()
        val newCall = Any()
        p.arm(owner = oldCall, on = true, platform = "x")
        p.arm(owner = newCall, on = true, platform = "x")
        assertEquals(listOf(1L), engine.freed)
        p.disarm(oldCall)
        p.process(3, 480, buffer())
        assertEquals(listOf(2L), engine.processed)
        p.disarm(newCall)
        assertEquals(listOf(1L, 2L), engine.freed)
    }

    @Test
    fun aSuppressorIsNeverFreedUnderTheAudioThread() {
        val engine = FakeEngine()
        val p = processor(engine, mutableListOf())
        p.initialize(48_000, 1)
        p.arm(owner = this, on = true, platform = "x")
        val gate = CountDownLatch(1)
        val entered = CountDownLatch(1)
        engine.gate = gate
        engine.entered = entered
        val audio = thread { p.process(3, 480, buffer()) }
        assertTrue(entered.await(5, TimeUnit.SECONDS))
        val control = thread { p.setEnabled(false) }
        Thread.sleep(50)
        synchronized(engine) { assertTrue("freed while in use", engine.freed.isEmpty()) }
        gate.countDown()
        audio.join(5_000)
        control.join(5_000)
        synchronized(engine) {
            assertEquals(listOf(1L), engine.processed)
            assertEquals(listOf(1L), engine.freed)
        }
    }
}
