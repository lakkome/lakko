// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.translation

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecordingConfiguration
import android.media.MediaCodec
import android.media.MediaExtractor
import android.media.MediaFormat
import android.media.MediaRecorder
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.speech.ModelDownloadListener
import android.speech.RecognitionListener
import android.speech.RecognitionSupport
import android.speech.RecognitionSupportCallback
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import android.util.Log
import androidx.annotation.RequiresApi
import androidx.core.content.ContextCompat
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import uniffi.lakko_ffi_uniffi.TranslationEngineState
import uniffi.lakko_ffi_uniffi.sameLanguage
import java.io.File
import java.io.OutputStream
import kotlin.coroutines.resume

/**
 * Transcription with Android's on-device speech recognizer (ADR-0082 section
 * 1): `SpeechRecognizer.createOnDeviceSpeechRecognizer`, and only after
 * `SpeechRecognizer.isOnDeviceRecognitionAvailable` said yes.
 * `createSpeechRecognizer` is never called: its own documentation says the
 * implementation "is likely to stream audio to remote servers". The
 * fail-closed source test refuses that call anywhere in the app.
 *
 * A voice message is a file, not the microphone. API 33 added
 * `RecognizerIntent.EXTRA_AUDIO_SOURCE` (a file descriptor of raw PCM, with
 * the channel count, encoding and sample rate extras), and a segmented
 * session that ends exactly when that audio is closed. Below API 33 there is
 * no way to hand the recognizer a file, so transcription is unavailable.
 *
 * The platform documentation says a recognizer that does not support
 * `EXTRA_AUDIO_SOURCE` "will open the mic for audio" instead. That would
 * record the room, not the message, so while a transcription runs the
 * device's active recordings are watched; if a microphone recording appears,
 * the session is cancelled and transcription is turned off on this device
 * for good ([micFallbackSeen]). Fail closed: a false positive (the person
 * started recording at the same moment) costs the feature, never privacy.
 */
@RequiresApi(Build.VERSION_CODES.TIRAMISU)
internal class OnDeviceSpeechTranscriber(context: Context) : TranscriptionEngine {
    private val appContext = context.applicationContext
    private val prefs = appContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun micFallbackSeen(): Boolean = prefs.getBoolean(KEY_MIC_FALLBACK, false)

    private fun hasPermission(): Boolean =
        ContextCompat.checkSelfPermission(appContext, Manifest.permission.RECORD_AUDIO) ==
            PackageManager.PERMISSION_GRANTED

    override suspend fun available(): Boolean = withContext(Dispatchers.Main) {
        !micFallbackSeen() && hasPermission() && SpeechRecognizer.isOnDeviceRecognitionAvailable(appContext)
    }

    private fun intentFor(language: String): Intent =
        Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
            putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
            putExtra(RecognizerIntent.EXTRA_LANGUAGE, language)
            putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true)
        }

    private suspend fun support(language: String): RecognitionSupport? {
        if (!available()) return null
        return withContext(Dispatchers.Main) {
            val recognizer = try {
                SpeechRecognizer.createOnDeviceSpeechRecognizer(appContext)
            } catch (_: Throwable) {
                return@withContext null
            }
            try {
                withTimeoutOrNull(TIMEOUT_MS) {
                    suspendCancellableCoroutine<RecognitionSupport?> { cont ->
                        recognizer.checkRecognitionSupport(
                            intentFor(language),
                            ContextCompat.getMainExecutor(appContext),
                            object : RecognitionSupportCallback {
                                override fun onSupportResult(recognitionSupport: RecognitionSupport) {
                                    if (cont.isActive) cont.resume(recognitionSupport)
                                }

                                override fun onError(error: Int) {
                                    if (cont.isActive) cont.resume(null)
                                }
                            },
                        )
                    }
                }
            } finally {
                recognizer.destroy()
            }
        }
    }

    override suspend fun state(language: String): TranslationEngineState {
        val s = support(language) ?: return TranslationEngineState.UNAVAILABLE
        return when {
            s.installedOnDeviceLanguages.any { sameLanguage(it, language) } -> TranslationEngineState.READY
            s.pendingOnDeviceLanguages.any { sameLanguage(it, language) } ||
                s.supportedOnDeviceLanguages.any { sameLanguage(it, language) } -> TranslationEngineState.NEEDS_PACK
            // `onlineLanguages` are deliberately not read: online is not a
            // state this feature has.
            else -> TranslationEngineState.UNAVAILABLE
        }
    }

    override suspend fun languages(): List<String> {
        val s = support("en") ?: return emptyList()
        return (s.installedOnDeviceLanguages + s.supportedOnDeviceLanguages + s.pendingOnDeviceLanguages).distinct()
    }

    override suspend fun installedPacks(): List<PackInfo> {
        val s = support("en") ?: return emptyList()
        return s.installedOnDeviceLanguages.map {
            PackInfo(it, null, forTranslation = false, forTranscription = true, managedBySystem = true)
        }
    }

    override suspend fun download(language: String, progress: (Int?) -> Unit): PackOutcome {
        if (!available()) return PackOutcome.FAILED
        return withContext(Dispatchers.Main) {
            val recognizer = try {
                SpeechRecognizer.createOnDeviceSpeechRecognizer(appContext)
            } catch (_: Throwable) {
                return@withContext PackOutcome.FAILED
            }
            try {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
                    withTimeoutOrNull(DOWNLOAD_TIMEOUT_MS) {
                        suspendCancellableCoroutine<PackOutcome> { cont ->
                            recognizer.triggerModelDownload(
                                intentFor(language),
                                ContextCompat.getMainExecutor(appContext),
                                object : ModelDownloadListener {
                                    override fun onProgress(completedPercent: Int) = progress(completedPercent)

                                    override fun onSuccess() {
                                        if (cont.isActive) cont.resume(PackOutcome.DONE)
                                    }

                                    override fun onScheduled() {
                                        if (cont.isActive) cont.resume(PackOutcome.SCHEDULED)
                                    }

                                    override fun onError(error: Int) {
                                        if (cont.isActive) cont.resume(PackOutcome.FAILED)
                                    }
                                },
                            )
                        }
                    } ?: PackOutcome.SCHEDULED
                } else {
                    progress(null)
                    recognizer.triggerModelDownload(intentFor(language))
                    PackOutcome.SCHEDULED
                }
            } catch (_: Throwable) {
                PackOutcome.FAILED
            } finally {
                recognizer.destroy()
            }
        }
    }

    override suspend fun transcribe(file: File, language: String, partial: (String) -> Unit): TranscriptOutcome {
        if (!available()) return TranscriptOutcome.Failed
        val pipe = try {
            ParcelFileDescriptor.createPipe()
        } catch (_: Throwable) {
            return TranscriptOutcome.Failed
        }
        val readSide = pipe[0]
        val writeSide = pipe[1]
        val writer = Thread {
            ParcelFileDescriptor.AutoCloseOutputStream(writeSide).use { out ->
                try {
                    decodeToPcm16Mono(file, out)
                } catch (_: Throwable) {
                    // The session ends when the audio closes; a decode failure
                    // reads as a short or empty transcript, never as partial
                    // bytes of a file that did not open (the file was already
                    // authenticated by the media hub).
                }
            }
        }
        val outcome = withContext(Dispatchers.Main) {
            val recognizer = try {
                SpeechRecognizer.createOnDeviceSpeechRecognizer(appContext)
            } catch (_: Throwable) {
                return@withContext TranscriptOutcome.Failed
            }
            val audio = appContext.getSystemService(AudioManager::class.java)
            try {
                suspendCancellableCoroutine<TranscriptOutcome> { cont ->
                    val segments = mutableListOf<String>()
                    fun finish(o: TranscriptOutcome) {
                        if (cont.isActive) cont.resume(o)
                    }
                    // Recordings already running (a call, another app) are not
                    // this session's; only one that starts during it is.
                    val before = audio?.activeRecordingConfigurations
                        ?.map { it.clientAudioSessionId }?.toSet().orEmpty()
                    val micWatch = object : AudioManager.AudioRecordingCallback() {
                        override fun onRecordingConfigChanged(configs: MutableList<AudioRecordingConfiguration>) {
                            if (configs.any { it.clientAudioSource in MIC_SOURCES && it.clientAudioSessionId !in before }) {
                                prefs.edit().putBoolean(KEY_MIC_FALLBACK, true).apply()
                                Log.w(TAG, "transcription stopped code=mic-opened")
                                recognizer.cancel()
                                finish(TranscriptOutcome.Failed)
                            }
                        }
                    }
                    audio?.registerAudioRecordingCallback(micWatch, Handler(Looper.getMainLooper()))
                    cont.invokeOnCancellation {
                        audio?.unregisterAudioRecordingCallback(micWatch)
                        recognizer.cancel()
                    }
                    recognizer.setRecognitionListener(object : RecognitionListener {
                        override fun onReadyForSpeech(params: Bundle?) = Unit
                        override fun onBeginningOfSpeech() = Unit
                        override fun onRmsChanged(rmsdB: Float) = Unit
                        override fun onBufferReceived(buffer: ByteArray?) = Unit
                        override fun onEndOfSpeech() = Unit
                        override fun onEvent(eventType: Int, params: Bundle?) = Unit

                        override fun onPartialResults(partialResults: Bundle?) {
                            val p = best(partialResults) ?: return
                            partial(joinSegments(segments + p))
                        }

                        override fun onSegmentResults(segmentResults: Bundle) {
                            best(segmentResults)?.let { segments += it }
                            partial(joinSegments(segments))
                        }

                        override fun onEndOfSegmentedSession() {
                            audio?.unregisterAudioRecordingCallback(micWatch)
                            val text = joinSegments(segments)
                            finish(if (text.isBlank()) TranscriptOutcome.NoSpeech else TranscriptOutcome.Text(text))
                        }

                        override fun onResults(results: Bundle?) {
                            audio?.unregisterAudioRecordingCallback(micWatch)
                            best(results)?.let { segments += it }
                            val text = joinSegments(segments)
                            finish(if (text.isBlank()) TranscriptOutcome.NoSpeech else TranscriptOutcome.Text(text))
                        }

                        override fun onError(error: Int) {
                            audio?.unregisterAudioRecordingCallback(micWatch)
                            Log.w(TAG, "transcription failed code=$error")
                            finish(
                                when (error) {
                                    SpeechRecognizer.ERROR_NO_MATCH,
                                    SpeechRecognizer.ERROR_SPEECH_TIMEOUT,
                                    -> if (segments.isEmpty()) {
                                        TranscriptOutcome.NoSpeech
                                    } else {
                                        TranscriptOutcome.Text(joinSegments(segments))
                                    }
                                    SpeechRecognizer.ERROR_LANGUAGE_UNAVAILABLE -> TranscriptOutcome.NeedsPack
                                    else -> TranscriptOutcome.Failed
                                },
                            )
                        }
                    })
                    val intent = intentFor(language).apply {
                        putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
                        putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE, readSide)
                        putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_CHANNEL_COUNT, 1)
                        putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_ENCODING, AudioFormat.ENCODING_PCM_16BIT)
                        putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_SAMPLING_RATE, SAMPLE_RATE)
                        // The session ends when, and only when, the audio is closed.
                        putExtra(RecognizerIntent.EXTRA_SEGMENTED_SESSION, RecognizerIntent.EXTRA_AUDIO_SOURCE)
                    }
                    recognizer.startListening(intent)
                    writer.start()
                }
            } catch (_: Throwable) {
                TranscriptOutcome.Failed
            } finally {
                recognizer.destroy()
            }
        }
        try {
            readSide.close()
        } catch (_: Throwable) {
            // already closed
        }
        return outcome
    }

    private fun best(bundle: Bundle?): String? =
        bundle?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull()?.trim()?.takeIf { it.isNotEmpty() }

    private companion object {
        const val TAG = "LakkoTranscription"
        const val PREFS = "lakko.translation.engine"
        const val KEY_MIC_FALLBACK = "transcription_mic_fallback"
        const val TIMEOUT_MS = 10_000L
        const val DOWNLOAD_TIMEOUT_MS = 300_000L
        const val SAMPLE_RATE = 16_000
        val MIC_SOURCES = setOf(
            MediaRecorder.AudioSource.MIC,
            MediaRecorder.AudioSource.VOICE_RECOGNITION,
            MediaRecorder.AudioSource.VOICE_COMMUNICATION,
            MediaRecorder.AudioSource.CAMCORDER,
            MediaRecorder.AudioSource.UNPROCESSED,
            MediaRecorder.AudioSource.DEFAULT,
        )
    }
}

/** Segments of one session as one text. */
internal fun joinSegments(segments: List<String>): String =
    segments.map { it.trim() }.filter { it.isNotEmpty() }.joinToString(" ")

/**
 * Decode the first audio track of [file] (the AAC voice message, or the audio
 * of a video message) to 16-bit PCM, mono, at 16 kHz, written to [out].
 */
internal fun decodeToPcm16Mono(file: File, out: OutputStream) {
    val extractor = MediaExtractor()
    try {
        extractor.setDataSource(file.path)
        val track = (0 until extractor.trackCount).firstOrNull {
            extractor.getTrackFormat(it).getString(MediaFormat.KEY_MIME)?.startsWith("audio/") == true
        } ?: return
        extractor.selectTrack(track)
        val format = extractor.getTrackFormat(track)
        val mime = format.getString(MediaFormat.KEY_MIME) ?: return
        val codec = MediaCodec.createDecoderByType(mime)
        try {
            codec.configure(format, null, null, 0)
            codec.start()
            val info = MediaCodec.BufferInfo()
            var inputDone = false
            var rate = format.getInteger(MediaFormat.KEY_SAMPLE_RATE)
            var channels = format.getInteger(MediaFormat.KEY_CHANNEL_COUNT)
            val resampler = LinearResampler(16_000)
            while (true) {
                if (!inputDone) {
                    val inIndex = codec.dequeueInputBuffer(10_000)
                    if (inIndex >= 0) {
                        val buf = codec.getInputBuffer(inIndex) ?: break
                        val n = extractor.readSampleData(buf, 0)
                        if (n < 0) {
                            codec.queueInputBuffer(inIndex, 0, 0, 0, MediaCodec.BUFFER_FLAG_END_OF_STREAM)
                            inputDone = true
                        } else {
                            codec.queueInputBuffer(inIndex, 0, n, extractor.sampleTime, 0)
                            extractor.advance()
                        }
                    }
                }
                val outIndex = codec.dequeueOutputBuffer(info, 10_000)
                when {
                    outIndex == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> {
                        val f = codec.outputFormat
                        rate = f.getInteger(MediaFormat.KEY_SAMPLE_RATE)
                        channels = f.getInteger(MediaFormat.KEY_CHANNEL_COUNT)
                    }
                    outIndex >= 0 -> {
                        val buf = codec.getOutputBuffer(outIndex)
                        if (buf != null && info.size > 0) {
                            val bytes = ByteArray(info.size)
                            buf.position(info.offset)
                            buf.get(bytes)
                            out.write(resampler.push(pcm16ToMono(bytes, channels), rate))
                        }
                        codec.releaseOutputBuffer(outIndex, false)
                        if (info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) break
                    }
                }
            }
        } finally {
            try {
                codec.stop()
            } catch (_: Throwable) {
                // not started
            }
            codec.release()
        }
    } finally {
        extractor.release()
    }
}

/** Little-endian 16-bit interleaved PCM to mono samples (the channel mean). */
internal fun pcm16ToMono(bytes: ByteArray, channels: Int): ShortArray {
    val ch = channels.coerceAtLeast(1)
    val frames = bytes.size / (2 * ch)
    val out = ShortArray(frames)
    for (i in 0 until frames) {
        var sum = 0
        for (c in 0 until ch) {
            val o = (i * ch + c) * 2
            sum += (bytes[o].toInt() and 0xff) or (bytes[o + 1].toInt() shl 8)
        }
        out[i] = (sum / ch).toShort()
    }
    return out
}

/** A linear resampler to [targetRate], fed chunk by chunk. */
internal class LinearResampler(private val targetRate: Int) {
    private var position = 0.0
    private var last: Short = 0

    /** Resample [samples] at [sourceRate]; little-endian 16-bit bytes out. */
    fun push(samples: ShortArray, sourceRate: Int): ByteArray {
        if (samples.isEmpty() || sourceRate <= 0) return ByteArray(0)
        val step = sourceRate.toDouble() / targetRate
        val out = java.io.ByteArrayOutputStream()
        while (position < samples.size) {
            val i = position.toInt()
            val frac = position - i
            val a = if (i == 0) last.toDouble() else samples[i - 1].toDouble()
            val b = samples[i].toDouble()
            val v = (a + (b - a) * frac).toInt().coerceIn(Short.MIN_VALUE.toInt(), Short.MAX_VALUE.toInt())
            out.write(v and 0xff)
            out.write((v shr 8) and 0xff)
            position += step
        }
        position -= samples.size
        last = samples.last()
        return out.toByteArray()
    }
}

/** The transcription engine for this device. */
internal fun transcriptionEngineFor(context: Context): TranscriptionEngine =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
        OnDeviceSpeechTranscriber(context)
    } else {
        NoTranscriptionEngine
    }
