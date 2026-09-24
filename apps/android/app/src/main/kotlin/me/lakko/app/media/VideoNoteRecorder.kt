// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import android.annotation.SuppressLint
import android.content.Context
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.media.MediaExtractor
import android.media.MediaFormat
import android.media.MediaMetadataRetriever
import android.util.Log
import android.util.Rational
import android.view.Surface
import androidx.camera.core.CameraInfo
import androidx.camera.core.CameraSelector
import androidx.camera.core.DynamicRange
import androidx.camera.core.MirrorMode
import androidx.camera.core.Preview
import androidx.camera.core.UseCase
import androidx.camera.core.UseCaseGroup
import androidx.camera.core.ViewPort
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.video.ExperimentalPersistentRecording
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
 *     centred square; the selected profile's short side bounds that square.
 *     This is the documented CameraX mechanism; it has not been observed on a
 *     device in this change (nothing ran on one), which the receiving side
 *     tolerates: the bubble draws the clip through a circle either way.
 *   * **Format.** The quality is not asked for by name — it is chosen by
 *     reading each supported quality's `EncoderProfiles` and keeping only
 *     H.264 + AAC ([chooseVideoNoteProfile]). CameraX has no audio-codec
 *     setter: it takes the audio mime type from the selected quality's
 *     profile, so asking for `Quality.SD` blind is how this recorder shipped
 *     AMR-NB audio that iOS refuses to play at all. A camera with no
 *     interoperable profile yields `NO_COMPATIBLE_FORMAT` and records nothing.
 *   * **Verification.** The finished file's real track codecs are read back in
 *     [measureTake] and logged; a mismatch is a warning naming the actual mime
 *     types (nothing else about the take is logged).
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
 * **One take, one recorder, and it lets go of exactly what it took.** The
 * provider is a process singleton, so this class never calls `unbindAll()`; it
 * unbinds the use cases its own [CameraOwnership] ledger names, and a released
 * recorder can never bind again. That is what keeps the teardown of a finished
 * take — which does not happen instantly, see [finish] — from unbinding the
 * camera out from under the NEXT take.
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
    private var selectedQuality: Quality? = null

    /**
     * What THIS take bound, so [release] can let go of exactly that.
     *
     * Never `unbindAll()`: the provider is a process singleton, and this
     * recorder's teardown runs late enough (see [finish]) that a blanket
     * unbind reaches the NEXT take's camera. See [CameraOwnership].
     */
    private val ownership = CameraOwnership<UseCase>()

    /** The lens bound right now, or `null` before a successful bind. */
    public var lens: CameraLens? = null
        private set

    /** The lenses CameraX reported at the last [bind]. */
    public var availableLenses: Set<CameraLens> = emptySet()
        private set

    /** The outcome of [bind]. */
    public enum class BindResult {
        BOUND,
        NO_CAMERA,

        /** The camera offers no H.264 + AAC profile: recording is refused. */
        NO_COMPATIBLE_FORMAT,
        FAILED,
    }

    /**
     * Bind a camera to [view] and prepare the recorder: the front camera when
     * there is one, else the back camera, else any camera
     * ([chooseVideoNoteLens]), recording at the smallest H.264 + AAC profile
     * that camera offers ([chooseVideoNoteProfile]).
     */
    public suspend fun bind(view: PreviewView): BindResult {
        // A released take never touches the camera again: its use cases are
        // gone and anything it bound now would be owned by nobody.
        if (!ownership.mayBind()) {
            Log.w(TAG, "bind refused: this take has already released the camera")
            return BindResult.FAILED
        }
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
        val info = infos[chosen] ?: return BindResult.FAILED
        val profile = chooseVideoNoteProfile(videoNoteProfileOptions(info)) ?: run {
            // UX spec §8: refuse rather than send a clip only some clients play.
            Log.w(TAG, "no H.264 + AAC camcorder profile on this camera: video notes unavailable")
            return BindResult.NO_COMPATIBLE_FORMAT
        }
        Log.i(
            TAG,
            "video note profile: ${profile.width}x${profile.height} " +
                "video=${profile.videoMime} audio=${profile.audioMime}",
        )
        if (preview == null) {
            preview = Preview.Builder().build()
        }
        if (capture == null || selectedQuality != profile.quality) {
            // No FallbackStrategy on purpose: a fallback re-opens the defect
            // this selection closes by wandering onto a neighbouring quality
            // whose profile may carry AMR-NB audio.
            val recorder = Recorder.Builder()
                .setQualitySelector(QualitySelector.from(profile.quality))
                .build()
            capture = VideoCapture.Builder(recorder)
                .setMirrorMode(MirrorMode.MIRROR_MODE_ON_FRONT_ONLY)
                .build()
            selectedQuality = profile.quality
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
            // Exactly this take's two use cases, never `unbindAll()`: a flip
            // moves OUR preview and capture to the other lens and has no
            // business unbinding whatever else the process has bound.
            p.unbind(pv, vc)
            p.bindToLifecycle(lifecycleOwner, info.cameraSelector, group)
            ownership.onBound(listOf(pv, vc))
            lens = target
            true
        } catch (e: Exception) {
            Log.w(TAG, "camera bind failed: ${e.javaClass.simpleName}")
            runCatching { p.unbind(pv, vc) }
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
        // The recording is finalized, so the camera has nothing left to do:
        // let it go HERE, before the file is read. Reading it is not quick —
        // [measureTake] runs a `MediaExtractor` and a `MediaMetadataRetriever`
        // and then DECODES the first video frame and JPEG-compresses it, in a
        // loop, which on an emulator is seconds. Releasing after that left the
        // camera bound to a take that had already been sent, and the next hold
        // fell inside that window.
        release()
        if (!ok || file == null || !file.exists() || file.length() == 0L) {
            file?.delete()
            return null
        }
        // Metadata and poster decoding read the file: off the main thread.
        return withContext(Dispatchers.IO) { measureTake(file, elapsedMs) }
    }

    private fun measureTake(file: File, elapsedMs: Long): RecordedTake? {
        verifyCodecs(file)
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
        // `close()`, not `stop()`. They stop the same recording, but `close()`
        // is `Recording`'s `AutoCloseable` contract — "release the resources" —
        // and this recording is PERSISTENT, so the unbind that follows in
        // [release] is by design ignored by it. Whatever it still holds is let
        // go here or not at all.
        runCatching { rec?.close() }
        file?.delete()
    }

    /**
     * Let go of the camera. Safe to call more than once, and the second call
     * is INERT rather than destructive: it unbinds nothing, so a teardown that
     * lands late cannot take the camera away from a take that started since.
     */
    public fun release() {
        discard()
        val p = provider
        val ours = ownership.releaseBound()
        if (p != null && ours.isNotEmpty()) {
            runCatching { p.unbind(*ours.toTypedArray()) }
        }
        preview?.setSurfaceProvider(null)
        // Drop every handle: this recorder is done, and holding a `Preview` or
        // a `VideoCapture` past the unbind keeps its encoder and its surface
        // alive for as long as the instance is referenced.
        preview = null
        capture = null
        selectedQuality = null
        provider = null
        previewView = null
        lens = null
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
         * Every quality [info]'s camera supports, described by the codecs and
         * frame size its `EncoderProfiles` declares — the input
         * [chooseVideoNoteProfile] decides on. A quality whose profile cannot
         * be read is left out rather than guessed at.
         */
        internal fun videoNoteProfileOptions(info: CameraInfo): List<VideoNoteProfileOption<Quality>> {
            val caps = runCatching { Recorder.getVideoCapabilities(info) }.getOrElse {
                Log.w(TAG, "video capabilities unavailable: ${it.javaClass.simpleName}")
                return emptyList()
            }
            val range = DynamicRange.SDR
            val qualities = runCatching { caps.getSupportedQualities(range) }.getOrElse {
                Log.w(TAG, "quality enumeration failed: ${it.javaClass.simpleName}")
                return emptyList()
            }
            return qualities.mapNotNull { quality ->
                val profiles = runCatching { caps.getProfiles(quality, range) }.getOrNull()
                val video = profiles?.defaultVideoProfile ?: return@mapNotNull null
                VideoNoteProfileOption(
                    quality = quality,
                    videoMime = video.mediaType,
                    audioMime = profiles.defaultAudioProfile?.mediaType,
                    width = video.width,
                    height = video.height,
                )
            }
        }

        /**
         * Read the finished file's real track codecs back and say what they
         * are. The profile selection says what the device promised; this says
         * what it produced, and a device that produced something else is worth
         * a warning even though the take is still handed on (the user recorded
         * it; dropping it silently would be worse than a clip that may not
         * play everywhere). Only mime types are logged.
         */
        private fun verifyCodecs(file: File) {
            val extractor = MediaExtractor()
            var video: String? = null
            var audio: String? = null
            try {
                extractor.setDataSource(file.absolutePath)
                for (track in 0 until extractor.trackCount) {
                    val mime = runCatching { extractor.getTrackFormat(track).getString(MediaFormat.KEY_MIME) }
                        .getOrNull() ?: continue
                    when {
                        video == null && mime.startsWith("video/") -> video = mime
                        audio == null && mime.startsWith("audio/") -> audio = mime
                    }
                }
            } catch (e: Exception) {
                Log.w(TAG, "video note codec check failed: ${e.javaClass.simpleName}")
                return
            } finally {
                runCatching { extractor.release() }
            }
            if (videoNoteCodecsInteroperable(video, audio)) {
                Log.i(TAG, "video note codecs: video=$video audio=$audio")
            } else {
                Log.w(
                    TAG,
                    "video note codecs are not interoperable: video=$video audio=$audio " +
                        "(expected $VIDEO_NOTE_VIDEO_CODEC + $VIDEO_NOTE_AUDIO_CODEC)",
                )
            }
        }

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
