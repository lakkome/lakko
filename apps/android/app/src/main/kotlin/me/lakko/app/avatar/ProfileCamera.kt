// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.avatar

import android.annotation.SuppressLint
import android.content.Context
import android.util.Log
import android.util.Rational
import android.view.Surface
import androidx.camera.core.ImageCapture
import androidx.camera.core.ImageCaptureException
import androidx.camera.core.MirrorMode
import androidx.camera.core.Preview
import androidx.camera.core.UseCase
import androidx.camera.core.UseCaseGroup
import androidx.camera.core.ViewPort
import androidx.camera.lifecycle.ProcessCameraProvider
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
import kotlinx.coroutines.suspendCancellableCoroutine
import me.lakko.app.media.CameraLens
import me.lakko.app.media.CameraOwnership
import me.lakko.app.media.VideoNoteRecorder
import me.lakko.app.media.chooseVideoNoteLens
import me.lakko.app.media.flipTarget
import java.io.File
import kotlin.coroutines.resume

/**
 * docs/ux/profile-screen.md §2.3 — Lakko's own camera for the profile photo,
 * on the video-note capture code: the same process CameraX provider
 * ([VideoNoteRecorder.obtainProvider]), the same lens choice (front, else
 * back, else any; [chooseVideoNoteLens] / [flipTarget]) and the same
 * ownership ledger ([CameraOwnership]: this screen unbinds exactly what it
 * bound, never `unbindAll()`).
 *
 * Photo mode binds a preview and an `ImageCapture`; video mode a preview and
 * a `VideoCapture` recording WITHOUT sound (a profile video never carries
 * one, so the microphone is never asked for). Both are cut square by a 1:1
 * viewport. The take then goes to the crop screen, which re-encodes it.
 *
 * Threading: [bind], [flip], [release] on the main thread.
 */
@SuppressLint("MissingPermission")
public class ProfileCamera(
    private val context: Context,
    private val lifecycleOwner: LifecycleOwner,
) {
    public enum class Mode { PHOTO, VIDEO }

    public enum class BindResult { BOUND, NO_CAMERA, FAILED }

    private var provider: ProcessCameraProvider? = null
    private var preview: Preview? = null
    private var imageCapture: ImageCapture? = null
    private var videoCapture: VideoCapture<Recorder>? = null
    private var bound: List<UseCase> = emptyList()
    private var recording: Recording? = null
    private var finalized: CompletableDeferred<Boolean>? = null
    private val ownership = CameraOwnership<UseCase>()

    public var lens: CameraLens? = null
        private set

    public var availableLenses: Set<CameraLens> = emptySet()
        private set

    public var mode: Mode = Mode.PHOTO
        private set

    public val canFlip: Boolean get() = lens?.let { flipTarget(availableLenses, it) } != null

    /** Bind the camera to [view] in [mode]. */
    public suspend fun bind(view: PreviewView, mode: Mode): BindResult {
        if (!ownership.mayBind()) return BindResult.FAILED
        val p = VideoNoteRecorder.obtainProvider(context) ?: return if (VideoNoteRecorder.hasAnyCamera(context)) {
            BindResult.FAILED
        } else {
            BindResult.NO_CAMERA
        }
        provider = p
        val infos = VideoNoteRecorder.cameraInfos(p)
        availableLenses = infos.keys
        val chosen = lens ?: chooseVideoNoteLens(infos.keys) ?: return BindResult.NO_CAMERA
        if (preview == null) preview = Preview.Builder().build()
        preview?.setSurfaceProvider(view.surfaceProvider)
        this.mode = mode
        return if (rebind(p, chosen)) BindResult.BOUND else BindResult.FAILED
    }

    /** Switch between photo and video. Not while recording. */
    public fun setMode(mode: Mode): Boolean {
        if (recording != null) return false
        val p = provider ?: return false
        val current = lens ?: return false
        this.mode = mode
        return rebind(p, current)
    }

    /** Switch between the front and back camera. Not while recording. */
    public fun flip(): Boolean {
        if (recording != null) return false
        val p = provider ?: return false
        val current = lens ?: return false
        val target = flipTarget(availableLenses, current) ?: return false
        if (rebind(p, target)) return true
        rebind(p, current)
        return false
    }

    private fun rebind(p: ProcessCameraProvider, target: CameraLens): Boolean {
        val pv = preview ?: return false
        val capture: UseCase = when (mode) {
            Mode.PHOTO -> imageCapture ?: ImageCapture.Builder()
                .setCaptureMode(ImageCapture.CAPTURE_MODE_MINIMIZE_LATENCY)
                .build().also { imageCapture = it }
            Mode.VIDEO -> videoCapture ?: VideoCapture.Builder(
                Recorder.Builder()
                    .setQualitySelector(
                        QualitySelector.from(Quality.SD, FallbackStrategy.higherQualityOrLowerThan(Quality.SD)),
                    )
                    .build(),
            ).setMirrorMode(MirrorMode.MIRROR_MODE_ON_FRONT_ONLY).build().also { videoCapture = it }
        }
        return try {
            val info = VideoNoteRecorder.cameraInfos(p)[target] ?: return false
            if (bound.isNotEmpty()) p.unbind(*bound.toTypedArray())
            val group = UseCaseGroup.Builder()
                .setViewPort(ViewPort.Builder(Rational(1, 1), Surface.ROTATION_0).build())
                .addUseCase(pv)
                .addUseCase(capture)
                .build()
            p.bindToLifecycle(lifecycleOwner, info.cameraSelector, group)
            bound = listOf(pv, capture)
            ownership.onBound(bound)
            lens = target
            true
        } catch (e: Exception) {
            Log.w(TAG, "profile camera bind failed: ${e.javaClass.simpleName}")
            runCatching { p.unbind(pv, capture) }
            bound = emptyList()
            false
        }
    }

    /** Take a picture into [file]. */
    public suspend fun takePhoto(file: File): Boolean {
        val capture = imageCapture ?: return false
        if (mode != Mode.PHOTO) return false
        return suspendCancellableCoroutine { cont ->
            try {
                capture.takePicture(
                    ImageCapture.OutputFileOptions.Builder(file).build(),
                    ContextCompat.getMainExecutor(context),
                    object : ImageCapture.OnImageSavedCallback {
                        override fun onImageSaved(output: ImageCapture.OutputFileResults) {
                            if (cont.isActive) cont.resume(true)
                        }

                        override fun onError(exception: ImageCaptureException) {
                            Log.w(TAG, "profile photo failed: ${exception.imageCaptureError}")
                            if (cont.isActive) cont.resume(false)
                        }
                    },
                )
            } catch (e: Exception) {
                Log.w(TAG, "profile photo did not start: ${e.javaClass.simpleName}")
                if (cont.isActive) cont.resume(false)
            }
        }
    }

    /** Start recording (no sound) into [file]. */
    public fun startVideo(file: File): Boolean {
        val vc = videoCapture ?: return false
        if (mode != Mode.VIDEO || recording != null) return false
        val done = CompletableDeferred<Boolean>()
        return try {
            recording = vc.output
                .prepareRecording(context, FileOutputOptions.Builder(file).build())
                .start(ContextCompat.getMainExecutor(context)) { event ->
                    if (event is VideoRecordEvent.Finalize) {
                        done.complete(
                            !event.hasError() ||
                                event.error == VideoRecordEvent.Finalize.ERROR_DURATION_LIMIT_REACHED,
                        )
                    }
                }
            finalized = done
            true
        } catch (e: Exception) {
            Log.w(TAG, "profile video did not start: ${e.javaClass.simpleName}")
            false
        }
    }

    /** Stop recording; `true` when a usable file was written. */
    public suspend fun stopVideo(): Boolean {
        val rec = recording ?: return false
        val done = finalized
        recording = null
        finalized = null
        runCatching { rec.stop() }
        return done?.await() ?: false
    }

    public val isRecording: Boolean get() = recording != null

    /** Let go of the camera; safe to call twice. */
    public fun release() {
        runCatching { recording?.close() }
        recording = null
        val p = provider
        val ours = ownership.releaseBound()
        if (p != null && ours.isNotEmpty()) runCatching { p.unbind(*ours.toTypedArray()) }
        preview?.setSurfaceProvider(null)
        preview = null
        imageCapture = null
        videoCapture = null
        bound = emptyList()
        provider = null
    }

    private companion object {
        const val TAG = "LakkoProfileCamera"
    }
}
