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
import androidx.camera.core.CameraInfo
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
import kotlinx.coroutines.withTimeoutOrNull
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
 *   * **Camera choice.** The front camera, else the back camera, else any
 *     camera ([chooseVideoNoteLens]); [bind] reports `NO_CAMERA` only when
 *     CameraX lists none, so a device with a single rear camera records.
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

    /** The lens bound right now, or `null` before a successful bind. */
    public var lens: CameraLens? = null
        private set

    /** The lenses CameraX reported at the last [bind]. */
    public var availableLenses: Set<CameraLens> = emptySet()
        private set

    /** The outcome of [bind]. */
    public enum class BindResult { BOUND, NO_CAMERA, FAILED }

    /**
     * Bind a camera to [view] and prepare the recorder: the front camera when
     * there is one, else the back camera, else any camera
     * ([chooseVideoNoteLens]).
     */
    public suspend fun bind(view: PreviewView): BindResult {
        previewView = view
        val p = obtainProvider(context) ?: return if (hasAnyCamera(context)) {
            BindResult.FAILED
        } else {
            BindResult.NO_CAMERA
        }
        provider = p
        val infos = cameraInfos(p)
        availableLenses = infos.keys
        val chosen = chooseVideoNoteLens(infos.keys) ?: run {
            Log.w(TAG, "no camera available to bind")
            return BindResult.NO_CAMERA
        }
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
        return if (rebind(p, chosen)) BindResult.BOUND else BindResult.FAILED
    }

    private fun rebind(p: ProcessCameraProvider, target: CameraLens): Boolean {
        val pv = preview ?: return false
        val vc = capture ?: return false
        return try {
            val info = cameraInfos(p)[target] ?: return false
            val group = UseCaseGroup.Builder()
                .setViewPort(ViewPort.Builder(Rational(1, 1), Surface.ROTATION_0).build())
                .addUseCase(pv)
                .addUseCase(vc)
                .build()
            p.unbindAll()
            p.bindToLifecycle(lifecycleOwner, info.cameraSelector, group)
            lens = target
            true
        } catch (e: Exception) {
            Log.w(TAG, "camera bind failed: ${e.javaClass.simpleName}")
            runCatching { p.unbindAll() }
            lens = null
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

    /** `true` when there is another built-in lens to flip to. */
    public val canFlip: Boolean
        get() = lens?.let { flipTarget(availableLenses, it) } != null

    /** Switch between the front and back camera; a running recording goes on. */
    public fun flip(): Boolean {
        val p = provider ?: return false
        val current = lens ?: return false
        val target = flipTarget(availableLenses, current) ?: return false
        if (rebind(p, target)) return true
        rebind(p, current)
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

    public companion object {
        private const val TAG = "LakkoVideoNoteRecorder"
        public const val VIDEO_NOTE_MIME: String = "video/mp4"

        /** ADR-0047 §2 — square, at most this many pixels a side. */
        public const val VIDEO_NOTE_MAX_SIDE: Int = 480

        /** Poster side, in px — the bubble is about 240 dp. */
        public const val POSTER_SIDE: Int = 240

        /** A CameraX provider that has not come up in this long is treated as
         *  failed: the take must not sit in a recording UI with no camera. */
        private const val PROVIDER_TIMEOUT_MS = 6_000L

        public fun hasAnyCamera(context: Context): Boolean =
            context.packageManager.hasSystemFeature(PackageManager.FEATURE_CAMERA_ANY)

        /**
         * The process CameraX provider, or `null` when initialization failed
         * or did not finish within [PROVIDER_TIMEOUT_MS]. CameraX initializes
         * once per process; calling this early (when the composer enters
         * camera mode) warms it so a hold finds it ready.
         */
        public suspend fun obtainProvider(context: Context): ProcessCameraProvider? =
            withTimeoutOrNull(PROVIDER_TIMEOUT_MS) {
                suspendCancellableCoroutine { cont ->
                    val future = ProcessCameraProvider.getInstance(context)
                    future.addListener(
                        {
                            val result = runCatching { future.get() }
                            result.exceptionOrNull()?.let {
                                Log.w(TAG, "camera provider unavailable: ${it.javaClass.simpleName}")
                            }
                            cont.resume(result.getOrNull())
                        },
                        ContextCompat.getMainExecutor(context),
                    )
                }
            }

        /** The cameras CameraX can bind, one per facing (the first wins). */
        public fun cameraInfos(p: ProcessCameraProvider): Map<CameraLens, CameraInfo> {
            val infos = runCatching { p.availableCameraInfos }.getOrElse {
                Log.w(TAG, "camera enumeration failed: ${it.javaClass.simpleName}")
                emptyList()
            }
            val out = LinkedHashMap<CameraLens, CameraInfo>()
            for (info in infos) {
                val facing = when (runCatching { info.lensFacing }.getOrNull()) {
                    CameraSelector.LENS_FACING_FRONT -> CameraLens.FRONT
                    CameraSelector.LENS_FACING_BACK -> CameraLens.BACK
                    else -> CameraLens.OTHER
                }
                out.putIfAbsent(facing, info)
            }
            return out
        }

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
