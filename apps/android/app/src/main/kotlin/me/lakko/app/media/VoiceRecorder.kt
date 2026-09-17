// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import android.content.Context
import android.media.MediaMetadataRetriever
import android.media.MediaRecorder
import android.os.Build
import android.os.SystemClock
import android.util.Log
import java.io.File

/**
 * ADR-0047 §2 — a voice take: AAC-LC in MPEG-4 (`audio/mp4`), the one voice
 * format every client records and plays, via the platform `MediaRecorder`.
 *
 * Owns one recorder for one take. [sampleLevel] is called on the controller's
 * tick; each reading is kept for the envelope waveform
 * ([downsampleWaveform]) and returned for the live level meter.
 *
 * A failure anywhere reports `false` / `null` rather than throwing: the
 * controller turns that into a `capture-failed` event, which discards the take.
 * Nothing partial is ever handed back as a finished take — `MediaRecorder.stop`
 * throws when no valid data was written, and that is a discard.
 *
 * Thread-confined: every method is called on [RecorderThread.dispatcher],
 * never the main thread (they block on binder calls).
 */
public class VoiceRecorder(private val context: Context) {

    private var recorder: MediaRecorder? = null
    private var output: File? = null
    private var startedAt: Long = 0L
    private val levels = ArrayList<Double>()

    /** Start recording into [file]. `false` when the recorder could not start
     *  (no microphone, another app holds it, permission missing). */
    public fun start(file: File, onError: () -> Unit): Boolean {
        release()
        val r = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            MediaRecorder(context)
        } else {
            @Suppress("DEPRECATION")
            MediaRecorder()
        }
        return try {
            r.setAudioSource(MediaRecorder.AudioSource.MIC)
            r.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4)
            r.setAudioEncoder(MediaRecorder.AudioEncoder.AAC)
            r.setAudioChannels(1)
            r.setAudioSamplingRate(SAMPLE_RATE)
            r.setAudioEncodingBitRate(BIT_RATE)
            r.setMaxDuration(VOICE_CAP_MS.toInt() + CAP_SLACK_MS)
            r.setOutputFile(file.absolutePath)
            r.setOnErrorListener { _, _, _ -> onError() }
            r.prepare()
            r.start()
            recorder = r
            output = file
            startedAt = SystemClock.elapsedRealtime()
            levels.clear()
            true
        } catch (e: Exception) {
            Log.w(TAG, "voice recorder did not start: ${e.javaClass.simpleName}")
            runCatching { r.release() }
            file.delete()
            false
        }
    }

    /** Current level 0..1; also appended to the waveform series. */
    public fun sampleLevel(): Double {
        val r = recorder ?: return 0.0
        val level = runCatching { levelFromMaxAmplitude(r.maxAmplitude) }.getOrDefault(0.0)
        levels.add(level)
        return level
    }

    /**
     * Stop and hand back the finished take, or `null` when nothing usable was
     * recorded (the file is then deleted).
     */
    public fun finish(): RecordedTake? {
        val r = recorder ?: return null
        val file = output
        val elapsed = SystemClock.elapsedRealtime() - startedAt
        recorder = null
        output = null
        val ok = try {
            r.stop()
            true
        } catch (_: RuntimeException) {
            false
        } finally {
            runCatching { r.release() }
        }
        if (!ok || file == null || !file.exists() || file.length() == 0L) {
            file?.delete()
            return null
        }
        val duration = measuredDurationMs(file) ?: elapsed
        return RecordedTake(
            kind = CaptureKind.VOICE,
            file = file,
            mime = VOICE_MIME,
            durationMs = duration.coerceIn(0L, VOICE_CAP_MS),
            waveform = downsampleWaveform(levels.toList()),
            width = null,
            height = null,
            poster = null,
        )
    }

    /** Stop and delete whatever was recorded. */
    public fun discard() {
        val file = output
        release()
        file?.delete()
    }

    private fun release() {
        val r = recorder ?: return
        recorder = null
        output = null
        runCatching { r.stop() }
        runCatching { r.release() }
    }

    public companion object {
        private const val TAG = "LakkoVoiceRecorder"
        public const val VOICE_MIME: String = "audio/mp4"
        private const val SAMPLE_RATE = 44_100
        private const val BIT_RATE = 64_000

        /** The controller's tick stops the take at the cap; this only keeps the
         *  recorder itself from stopping a hair earlier. */
        private const val CAP_SLACK_MS = 2_000

        internal fun measuredDurationMs(file: File): Long? {
            val retriever = MediaMetadataRetriever()
            return try {
                retriever.setDataSource(file.absolutePath)
                retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)?.toLongOrNull()
            } catch (_: Exception) {
                null
            } finally {
                runCatching { retriever.release() }
            }
        }
    }
}

/** A finished take, ready for preview or sending. */
public data class RecordedTake(
    val kind: CaptureKind,
    val file: File,
    val mime: String,
    val durationMs: Long,
    val waveform: ByteArray?,
    val width: Int?,
    val height: Int?,
    /** A JPEG poster frame for a video note (<= 256 KiB), or `null`. */
    val poster: PosterFrame?,
)

public class PosterFrame(val bytes: ByteArray, val mime: String, val width: Int, val height: Int)
