// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import android.util.Log
import java.nio.ByteBuffer
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import me.lakko.sframe.NativeNoiseSuppressor
import org.webrtc.ExternalAudioProcessingFactory

/**
 * Lakko's capture noise suppressor on Android (ADR-0088,
 * `docs/ux/calls.md` section 4.1.3): RNNoise on the microphone after the
 * platform's echo canceller and before the encoder, so before SFrame.
 *
 * ## Where it runs
 *
 * libwebrtc's capture post-processing stage. The process-wide
 * `PeerConnectionFactory` is built with an
 * `org.webrtc.ExternalAudioProcessingFactory` whose capture
 * post-processing is [CaptureNoiseProcessor]. In the pinned
 * `io.github.webrtc-sdk:android` 125.6422.07 that stage is a
 * `webrtc::CustomProcessing` the audio processing module runs on its full
 * band capture buffer after echo cancellation, noise suppression and gain
 * control (M125 `AudioProcessingImpl::ProcessCaptureStreamLocked`); the
 * SDK's adapter (`external_audio_processor.cc`, m125_release) hands Kotlin a
 * direct `ByteBuffer` over channel 0 of that buffer, 10 ms at a time, as
 * `f32` in the 16-bit range, under its own mutex, on libwebrtc's audio
 * thread. On phones where the platform's AEC / NS effects run on the
 * `AudioRecord` (the Pixel's case), they ran even earlier.
 *
 * ## Sample rate
 *
 * The model runs at 48 kHz. A stage at 24, 16, 12 or 8 kHz is raised to
 * 48 kHz by the Rust core, cleaned and lowered back (ADR-0088 amendment of
 * 2026-11-10: a Pixel 7a opened the stage at 16 kHz before settling at
 * 48 kHz, and until then RNNoise sat out). Any other rate (32 or 44.1 kHz)
 * and any stage with more than one channel is passed through untouched,
 * and the format line says so. With the default audio module the stage
 * runs at the capture rate rounded up to 16 / 32 / 48 kHz (M125
 * `SuitableProcessRate`), so a phone recording at 48 kHz gets 48 kHz.
 *
 * ## Failure
 *
 * Anything that goes wrong leaves the call on the platform's processing,
 * as with the switch off, and logs one line: the native library or its
 * noise functions missing (the hook is then not installed at all), the
 * hook refused, a suppressor that cannot be made, a buffer the native side
 * refuses (the buffer is then untouched and the rest of the call passes
 * through).
 */
internal object CaptureNoiseSuppression {
    private const val TAG = "CaptureNoise"

    val processor: CaptureNoiseProcessor = CaptureNoiseProcessor(
        engine = NativeNoiseEngine,
        log = { line -> Log.i(TAG, line) },
    )

    /**
     * The audio processing factory the shared `PeerConnectionFactory` is
     * built with, or `null` to build it as before (no native suppressor in
     * this build, or the SDK hook refused). Called once per process, when
     * the factory is built.
     */
    fun audioProcessingFactory(): ExternalAudioProcessingFactory? {
        if (NativeNoiseEngine.sampleRate == 0) {
            Log.i(TAG, "noise suppression unavailable: no native suppressor in libsframe_jni.so; platform processing only")
            return null
        }
        return try {
            val factory = ExternalAudioProcessingFactory()
            factory.setCapturePostProcessing(processor)
            Log.i(TAG, "noise suppression hook installed (libwebrtc capture post-processing)")
            factory
        } catch (cause: Throwable) {
            Log.w(TAG, "noise suppression hook not installed, platform processing only: ${cause.message}")
            null
        }
    }

    /** Platform effects the audio module uses, for the log line. */
    fun platformEffects(): String = try {
        val aec = org.webrtc.audio.JavaAudioDeviceModule.isBuiltInAcousticEchoCancelerSupported()
        val ns = org.webrtc.audio.JavaAudioDeviceModule.isBuiltInNoiseSuppressorSupported()
        "platform AEC ${if (aec) "hardware" else "libwebrtc software"}, " +
            "platform NS ${if (ns) "hardware" else "libwebrtc software"}"
    } catch (cause: Throwable) {
        "platform effects unknown"
    }
}

/** The native suppressor, as [CaptureNoiseProcessor] uses it. */
internal interface NoiseEngine {
    /** 48 000, or `0` when there is no native suppressor. */
    val sampleRate: Int

    /** A fresh suppressor handle, or `0`. Control thread. */
    fun create(): Long

    /** Frees [handle]; `0` is a no-op. Control thread. */
    fun free(handle: Long)

    /** Whether [processAtRate] takes [rateHz] (48 000, 24 000, 16 000,
     *  12 000, 8 000). */
    fun supportsRate(rateHz: Int): Boolean

    /** Cleans [frames] floats of a mono stream at [rateHz] in [buffer] in
     *  place; `0` on success. */
    fun processAtRate(handle: Long, rateHz: Int, buffer: ByteBuffer, frames: Int): Int
}

internal object NativeNoiseEngine : NoiseEngine {
    override val sampleRate: Int get() = NativeNoiseSuppressor.sampleRate
    override fun create(): Long = NativeNoiseSuppressor.create()
    override fun free(handle: Long) = NativeNoiseSuppressor.free(handle)
    override fun supportsRate(rateHz: Int): Boolean = NativeNoiseSuppressor.supportsRate(rateHz)
    override fun processAtRate(handle: Long, rateHz: Int, buffer: ByteBuffer, frames: Int): Int =
        NativeNoiseSuppressor.processAtRate(handle, rateHz, buffer, frames)
}

/**
 * The capture post-processing stage: cleans the microphone when the switch
 * is on and the stage runs mono at a rate the native suppressor takes, in
 * 10 ms blocks, else leaves the buffer as it is.
 *
 * Threads. [initialize] / [reset] / [process] are libwebrtc's, serialised by
 * the SDK adapter's mutex. [arm] / [disarm] / [setEnabled] are the app's,
 * serialised by [control], a lock the audio thread never takes. They share
 * only atomics and volatiles. [process] allocates nothing, takes no lock and
 * never throws (a throw would abort the process inside libwebrtc's JNI
 * glue); only its first failure logs, once.
 *
 * A fresh native suppressor is made for every call and every switch-on, so
 * no audio held in the suppressor's 10 ms delay or its overlap memory from
 * an earlier call reaches a new one. A suppressor is freed only after the
 * audio thread has been seen outside [process] once the handle was taken
 * away ([inProcess] reaches 0); if that never happens within the wait, the
 * handle is leaked rather than freed under the audio thread.
 */
internal class CaptureNoiseProcessor(
    private val engine: NoiseEngine,
    private val log: (String) -> Unit,
    private val pause: () -> Unit = { Thread.sleep(1) },
) : ExternalAudioProcessingFactory.AudioProcessing {

    private val handle = AtomicLong(0)
    private val inProcess = AtomicInteger(0)
    private val failureLogged = AtomicBoolean(false)
    private val cleaned = AtomicLong(0)
    private val passed = AtomicLong(0)

    @Volatile private var enabled = false
    @Volatile private var failed = false
    @Volatile private var rateHz = 0
    @Volatile private var channels = 0

    /** [rateHz] is one the native suppressor takes; decided when the
     *  format is reported, so [process] does not ask on every block. */
    @Volatile private var rateSupported = false

    private val control = Any()
    private var owner: Any? = null
    private var formatLogged: Pair<Int, Int>? = null

    /** `true` while an armed call has the suppressor running. */
    val running: Boolean get() = enabled && !failed && handle.get() != 0L

    // ---- libwebrtc side -------------------------------------------------

    override fun initialize(sampleRateHz: Int, numChannels: Int) {
        try {
            rateHz = sampleRateHz
            channels = numChannels
            rateSupported = engine.sampleRate != 0 && engine.supportsRate(sampleRateHz)
            logFormat()
        } catch (_: Throwable) {
            // Never throw into libwebrtc.
        }
    }

    override fun reset(newRate: Int) {
        try {
            rateHz = newRate
            rateSupported = engine.sampleRate != 0 && engine.supportsRate(newRate)
            logFormat()
        } catch (_: Throwable) {
            // Never throw into libwebrtc.
        }
    }

    override fun process(numBands: Int, numFrames: Int, buffer: ByteBuffer) {
        try {
            val rate = rateHz
            if (!enabled || failed || !rateSupported || channels != 1 || numFrames * 100 != rate) {
                passed.incrementAndGet()
                return
            }
            inProcess.incrementAndGet()
            try {
                val h = handle.get()
                if (h == 0L) {
                    passed.incrementAndGet()
                    return
                }
                val status = engine.processAtRate(h, rate, buffer, numFrames)
                if (status == NativeNoiseSuppressor.STATUS_OK) {
                    cleaned.incrementAndGet()
                } else {
                    failed = true
                    passed.incrementAndGet()
                    if (failureLogged.compareAndSet(false, true)) {
                        log("noise suppression failed (native status $status); platform processing only for the rest of the call")
                    }
                }
            } finally {
                inProcess.decrementAndGet()
            }
        } catch (cause: Throwable) {
            failed = true
            if (failureLogged.compareAndSet(false, true)) {
                try {
                    log("noise suppression failed (${cause.javaClass.simpleName}); platform processing only for the rest of the call")
                } catch (_: Throwable) {
                    // Never throw into libwebrtc.
                }
            }
        }
    }

    // ---- app side ---------------------------------------------------------

    /**
     * A call's microphone starts: a fresh suppressor when [on], the
     * counters cleared, and one line saying which processing applies.
     * [owner] is the call's connection; a later [arm] takes over.
     */
    fun arm(owner: Any, on: Boolean, platform: String) {
        synchronized(control) {
            this.owner = owner
            failed = false
            failureLogged.set(false)
            cleaned.set(0)
            passed.set(0)
            enabled = false
            val fresh = if (on) engine.create() else 0L
            swapIn(fresh)
            enabled = fresh != 0L
            log("call audio: ${describe(on, fresh)}; $platform")
        }
    }

    /** [owner]'s call ended: the suppressor freed, a summary logged. */
    fun disarm(owner: Any) {
        synchronized(control) {
            if (this.owner !== owner) return
            this.owner = null
            enabled = false
            swapIn(0L)
            log("call audio ended: ${cleaned.get()} buffers cleaned by RNNoise, ${passed.get()} passed through")
        }
    }

    /** The switch changed: applies to the running call at once. */
    fun setEnabled(on: Boolean) {
        synchronized(control) {
            if (owner == null) return
            if (on && !enabled) {
                failed = false
                failureLogged.set(false)
                val fresh = engine.create()
                swapIn(fresh)
                enabled = fresh != 0L
                log("noise suppression switched on: ${describe(true, fresh)}")
            } else if (!on && enabled) {
                enabled = false
                swapIn(0L)
                log("noise suppression switched off: platform processing only")
            }
        }
    }

    private fun describe(on: Boolean, fresh: Long): String = when {
        !on -> "noise suppression off, platform processing only"
        fresh == 0L -> "noise suppression on but unavailable (no native suppressor), platform processing only"
        else -> "RNNoise noise suppression on after platform processing" + formatNote()
    }

    private fun formatNote(): String {
        val rate = rateHz
        val model = engine.sampleRate
        return when {
            rate == 0 -> " (capture format not reported yet)"
            channels != 1 -> " but bypassed: capture runs at $rate Hz, $channels ch; RNNoise takes mono only"
            !rateSupported -> " but bypassed: capture runs at $rate Hz mono; RNNoise takes $model Hz or a rate $model Hz is a whole multiple of"
            rate == model -> " (capture $rate Hz mono)"
            else -> " (capture $rate Hz mono, raised to $model Hz for RNNoise)"
        }
    }

    private fun logFormat() {
        val now = rateHz to channels
        if (formatLogged == now) return
        formatLogged = now
        val model = engine.sampleRate
        val note = when {
            now.second != 1 -> "RNNoise bypassed (mono only)"
            !rateSupported -> "RNNoise bypassed (takes $model Hz or a rate $model Hz is a whole multiple of)"
            now.first == model -> "RNNoise applies when the switch is on"
            else -> "RNNoise applies when the switch is on, raised to $model Hz"
        }
        log("capture post-processing format ${now.first} Hz, ${now.second} ch: $note")
    }

    /** Puts [fresh] in place and frees what it replaced, once unused. */
    private fun swapIn(fresh: Long) {
        val old = handle.getAndSet(fresh)
        if (old == 0L) return
        var waits = 0
        while (inProcess.get() != 0) {
            if (waits >= MAX_WAITS) {
                log("noise suppressor still in use after $MAX_WAITS waits; left allocated")
                return
            }
            pause()
            waits++
        }
        engine.free(old)
    }

    private companion object {
        const val MAX_WAITS = 200
    }
}

/**
 * The `Noise suppression` switch of the call's Settings sheet
 * (`docs/ux/calls.md` section 4.1.3). On by default; this device only, in
 * its own preferences file (not per account: it is about this phone's
 * microphone); never sent to anyone. A change applies to a running call at
 * once.
 */
internal object NoiseSuppressionSetting {
    private const val PREFS = "lakko_call_audio"
    private const val KEY = "noise_suppression"

    @Volatile private var prefs: android.content.SharedPreferences? = null
    private val state = MutableStateFlow(true)

    /** The switch, for the sheet. */
    val on: StateFlow<Boolean> = state.asStateFlow()

    /** Reads the stored choice once; later calls are no-ops. */
    fun attach(context: Context) {
        if (prefs != null) return
        synchronized(this) {
            if (prefs != null) return
            val p = try {
                context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            } catch (cause: Throwable) {
                null
            }
            state.value = p?.getBoolean(KEY, true) ?: true
            prefs = p
        }
    }

    /** Sets the switch: stored, shown, and applied to a running call. */
    fun set(on: Boolean) {
        state.value = on
        try {
            prefs?.edit()?.putBoolean(KEY, on)?.apply()
        } catch (cause: Throwable) {
            // The choice still holds for this process.
        }
        CaptureNoiseSuppression.processor.setEnabled(on)
    }
}
