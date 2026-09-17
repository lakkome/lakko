// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import android.annotation.SuppressLint
import android.content.Context
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.media.MediaMetadataRetriever
import android.util.Log
import android.util.Rational
import android.view.Surface
import androidx.camera.core.CameraSelector
import androidx.camera.core.MirrorMode
import androidx.camera.core.Preview
import androidx.camera.core.UseCaseGroup
import androidx.camera.core.ViewPort
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.video.ExperimentalPersistentRecording
import androidx.camera.video.FallbackStrategy
import androidx.camera.video.FileOutputOptions
import androidx.camera.video.Quality
import androidx.camera.video.QualitySelector
import androidx.camera.video.Recorder
import androidx.camera.video.Recording
import androidx.camera.video.VideoCapture
import androidx.camera.video.VideoRecordEvent
import androidx.camera.view.PreviewView
import androidx.core.content.ContextCompat
import androidx.lifecycle.LifecycleOwner
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import java.io.ByteArrayOutputStream
import java.io.File
import kotlin.coroutines.resume

/**
 * ADR-0047 §2 — a video-note take: H.264 + AAC in MPEG-4 (`video/mp4`),
 * square, at most [VIDEO_NOTE_MAX_SIDE] px, recorded with CameraX
 * `Recorder` / `VideoCapture`.
 *
 *   * **Square.** The preview and the video capture are bound in one
 *     `UseCaseGroup` with a 1:1 `ViewPort`, so CameraX crops both to the same
 *     centred square. SD quality (480p) then bounds the square's side at 480.
 *     This is the documented CameraX mechanism; it has not been observed on a
 *     device in this change (nothing ran on one), which the receiving side
 *     tolerates: the bubble draws the clip through a circle either way.
 *   * **Flip while locked.** The recording is started as a PERSISTENT
 *     recording (CameraX 1.4), which keeps running when `VideoCapture` is
 *     rebound to the other camera. Without it, a flip would finalize the take.
 *   * **Poster.** After the take is finalized, [posterFrom] pulls the first
 *     frame and compresses it to a JPEG of at most 256 KiB, which travels as
 *     the note's encrypted thumb.
 *
 * Every failure is reported as `false` / `null`; the controller discards the
 * take on a failure, it never sends a partial one.
 *
 * Threading: CameraX requires [bind], [flip] and [release] on the main thread,
 * and its `Recording.start` / `stop` are asynchronous (the encoder runs on
 * CameraX's own executors, completion arrives via the Finalize event), so
 * none of them blocks main. The one blocking step, reading the finished file
 * for metadata and the poster frame, runs on `Dispatchers.IO` in [finish].
 */
@SuppressLint("MissingPermission") // The controller only starts after RECORD_AUDIO + CAMERA are granted.
@OptIn(ExperimentalPersistentRecording::class)
public class VideoNoteRecorder(
    private val context: Context,
    private val lifecycleOwner: LifecycleOwner,
) {
    private var provider: ProcessCameraProvider? = null
    private var preview: Preview? = null
    private var capture: VideoCapture<Recorder>? = null
    private var recording: Recording? = null
    private var output: File? = null
    private var finalized: CompletableDeferred<Boolean>? = null
    private var previewView: PreviewView? = null

    public var frontFacing: Boolean = true
        private set

    /** Bind the camera to [view] and prepare the recorder. */
    public suspend fun bind(view: PreviewView): Boolean {
        previewView = view
        val p = obtainProvider() ?: return false
        provider = p
        if (preview == null) {
            preview = Preview.Builder().build()
        }
        if (capture == null) {
            val recorder = Recorder.Builder()
                .setQualitySelector(
                    QualitySelector.from(
                        Quality.SD,
                        FallbackStrategy.lowerQualityOrHigherThan(Quality.SD),
                    ),
                )
                .build()
            capture = VideoCapture.Builder(recorder)
                .setMirrorMode(MirrorMode.MIRROR_MODE_ON_FRONT_ONLY)
                .build()
        }
        preview?.setSurfaceProvider(view.surfaceProvider)
        return rebind(p)
    }

    private fun rebind(p: ProcessCameraProvider): Boolean {
        val pv = preview ?: return false
        val vc = capture ?: return false
        val selector = if (frontFacing) CameraSelector.DEFAULT_FRONT_CAMERA else CameraSelector.DEFAULT_BACK_CAMERA
        return try {
            if (!p.hasCamera(selector)) return false
            val group = UseCaseGroup.Builder()
                .setViewPort(ViewPort.Builder(Rational(1, 1), Surface.ROTATION_0).build())
                .addUseCase(pv)
                .addUseCase(vc)
                .build()
            p.unbindAll()
            p.bindToLifecycle(lifecycleOwner, selector, group)
            true
        } catch (e: Exception) {
            Log.w(TAG, "camera bind failed: ${e.javaClass.simpleName}")
            false
        }
    }

    /** Start recording into [file]. [onError] fires if CameraX reports a
     *  recording error before a normal stop. */
    public fun start(file: File, onError: () -> Unit): Boolean {
        val vc = capture ?: return false
        val done = CompletableDeferred<Boolean>()
        return try {
            recording = vc.output
                .prepareRecording(context, FileOutputOptions.Builder(file).build())
                .asPersistentRecording()
                .withAudioEnabled()
                .start(ContextCompat.getMainExecutor(context)) { event ->
                    if (event is VideoRecordEvent.Finalize) {
                        val ok = !event.hasError() ||
                            event.error == VideoRecordEvent.Finalize.ERROR_DURATION_LIMIT_REACHED
                        if (!done.isCompleted && !ok && output != null) onError()
                        done.complete(ok)
                    }
                }
            output = file
            finalized = done
            true
        } catch (e: Exception) {
            Log.w(TAG, "video note recording did not start: ${e.javaClass.simpleName}")
            file.delete()
            false
        }
    }

    /** Switch between the front and back camera; a running recording goes on. */
    public fun flip(): Boolean {
        val p = provider ?: return false
        frontFacing = !frontFacing
        if (rebind(p)) return true
        frontFacing = !frontFacing
        rebind(p)
        return false
    }

    /** Stop and hand back the finished take, or `null` when nothing usable was
     *  recorded. */
    public suspend fun finish(elapsedMs: Long): RecordedTake? {
        val rec = recording ?: return null
        val file = output
        val done = finalized
        recording = null
        output = null
        finalized = null
        runCatching { rec.stop() }
        val ok = done?.await() ?: false
        if (!ok || file == null || !file.exists() || file.length() == 0L) {
            file?.delete()
            return null
        }
        // Metadata and poster decoding read the file: off the main thread.
        return withContext(Dispatchers.IO) { measureTake(file, elapsedMs) }
    }

    private fun measureTake(file: File, elapsedMs: Long): RecordedTake? {
        val retriever = MediaMetadataRetriever()
        return try {
            retriever.setDataSource(file.absolutePath)
            val duration = retriever
                .extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)
                ?.toLongOrNull() ?: elapsedMs
            val width = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_WIDTH)?.toIntOrNull()
            val height = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_HEIGHT)?.toIntOrNull()
            RecordedTake(
                kind = CaptureKind.VIDEO_NOTE,
                file = file,
                mime = VIDEO_NOTE_MIME,
                durationMs = duration.coerceIn(0L, VIDEO_NOTE_CAP_MS),
                waveform = null,
                width = width?.takeIf { it in 1..MAX_GIF_DIMENSION },
                height = height?.takeIf { it in 1..MAX_GIF_DIMENSION },
                poster = posterFrom(retriever),
            )
        } catch (_: Exception) {
            file.delete()
            null
        } finally {
            runCatching { retriever.release() }
        }
    }

    public fun discard() {
        val rec = recording
        val file = output
        recording = null
        output = null
        finalized = null
        runCatching { rec?.stop() }
        file?.delete()
    }

    /** Release the camera. Safe to call more than once. */
    public fun release() {
        discard()
        runCatching { provider?.unbindAll() }
        preview?.setSurfaceProvider(null)
        previewView = null
    }

    private suspend fun obtainProvider(): ProcessCameraProvider? =
        suspendCancellableCoroutine { cont ->
            val future = ProcessCameraProvider.getInstance(context)
            future.addListener(
                {
                    cont.resume(runCatching { future.get() }.getOrNull())
                },
                ContextCompat.getMainExecutor(context),
            )
        }

    public companion object {
        private const val TAG = "LakkoVideoNoteRecorder"
        public const val VIDEO_NOTE_MIME: String = "video/mp4"

        /** ADR-0047 §2 — square, at most this many pixels a side. */
        public const val VIDEO_NOTE_MAX_SIDE: Int = 480

        /** Poster side, in px — the bubble is about 240 dp. */
        public const val POSTER_SIDE: Int = 240

        public fun hasAnyCamera(context: Context): Boolean =
            context.packageManager.hasSystemFeature(PackageManager.FEATURE_CAMERA_ANY)

        /** First frame as a JPEG of at most [POSTER_MAX_BYTES] bytes. */
        internal fun posterFrom(retriever: MediaMetadataRetriever): PosterFrame? {
            val frame = runCatching { retriever.getFrameAtTime(0) }.getOrNull() ?: return null
            return try {
                val side = minOf(frame.width, frame.height)
                if (side <= 0) return null
                val square = Bitmap.createBitmap(
                    frame,
                    (frame.width - side) / 2,
                    (frame.height - side) / 2,
                    side,
                    side,
                )
                val target = minOf(side, POSTER_SIDE)
                val scaled = Bitmap.createScaledBitmap(square, target, target, true)
                var quality = 85
                var bytes: ByteArray
                do {
                    val out = ByteArrayOutputStream()
                    scaled.compress(Bitmap.CompressFormat.JPEG, quality, out)
                    bytes = out.toByteArray()
                    quality -= 15
                } while (bytes.size > POSTER_MAX_BYTES && quality > 10)
                if (bytes.size > POSTER_MAX_BYTES) null else PosterFrame(bytes, "image/jpeg", target, target)
            } catch (_: Exception) {
                null
            }
        }

        /** ADR-0047 §1 — the thumb blob bound (256 KiB). */
        private const val POSTER_MAX_BYTES = 256 * 1024
    }
}
