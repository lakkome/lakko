// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import android.util.Log
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.Executors
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import org.webrtc.VideoFrame
import org.webrtc.VideoSink

/**
 * The camera background on this device (`docs/ux/call-backgrounds.md`,
 * ADR-0089): the process-wide side of the per-camera [BackgroundStage].
 *
 *  - [status]: what the call screen and the Settings sheet show (UX
 *    section 3).
 *  - [failures]: one event per failure; the call screen turns the camera
 *    off through the same path as its camera button and shows the line.
 *  - the preview sinks of the Settings sheet, handed every frame a camera
 *    stage hands on (and nothing it drops);
 *  - the preview-only camera the sheet runs while the call's camera is off
 *    and none is being captured (UX section 2.2): it feeds no track, so
 *    nothing is sent.
 */
internal object CameraBackground {
    private const val TAG = "CameraBackground"

    private val mutableStatus = MutableStateFlow(BackgroundStatus.OFF)
    val status: StateFlow<BackgroundStatus> = mutableStatus.asStateFlow()

    private val mutableFailures = MutableSharedFlow<Unit>(
        extraBufferCapacity = 1,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )
    val failures: SharedFlow<Unit> = mutableFailures.asSharedFlow()

    /** Whether this device can run a background at all (the section is
     *  absent otherwise, UX section 3). */
    val available: Boolean get() = NativeSegmentEngine.available

    private val previewSinks = CopyOnWriteArrayList<VideoSink>()
    private val liveFans = CopyOnWriteArrayList<CapturerFan>()

    /** One thread for picture decoding and the built-in drawings. */
    private val pictureWorker = Executors.newSingleThreadExecutor { r ->
        Thread(r, "lakko-background-pictures").apply { isDaemon = true }
    }

    @Volatile private var appContext: Context? = null

    private val events = object : BackgroundEvents {
        override fun status(status: BackgroundStatus) {
            mutableStatus.value = status
        }

        override fun failed() {
            Log.w(TAG, "camera background stopped; the camera is turned off")
            mutableStatus.value = BackgroundStatus.FAILED
            mutableFailures.tryEmit(Unit)
        }
    }

    /** The stage for one camera. */
    fun newStage(context: Context): CameraFrameStage {
        appContext = context.applicationContext
        CameraBackgroundSetting.attach(context)
        val worker = Executors.newSingleThreadExecutor { r ->
            Thread(r, "lakko-background-model").apply { isDaemon = true }
        }
        val engine = NativeSegmentEngine
        val stage = BackgroundStage(
            engine = engine,
            compositor = BackgroundGlCompositor(
                modelWidth = engine.inputWidth,
                modelHeight = engine.inputHeight,
                pictureWorker = pictureWorker,
                pictures = pictureSource,
            ),
            selection = { CameraBackgroundSetting.holder.current() },
            generation = { CameraBackgroundSetting.holder.generation },
            lookOf = ::lookFor,
            worker = worker,
            events = events,
            clock = { android.os.SystemClock.elapsedRealtime() },
            paused = { !cameraOn && previewSinks.isEmpty() },
        )
        return ShutdownStage(stage) { worker.execute { worker.shutdown() } }
    }

    @Volatile private var photoStore: BackgroundPhotoStore? = null

    private fun photos(): BackgroundPhotoStore? =
        photoStore ?: appContext?.let { BackgroundPhotoStore.onDevice(it) }?.also { photoStore = it }

    /** [selection] as drawn now: a selected own photo that is gone is the blur. */
    fun lookFor(selection: BackgroundSelection): BackgroundLook? = backgroundLookFor(selection) { id ->
        val store = photos() ?: return@backgroundLookFor false
        val account = me.lakko.app.contacts.AcceptedContactsStore.boundAccountB64 ?: return@backgroundLookFor false
        store.exists(account, id)
    }

    private val pictureSource = object : BackgroundPictureSource {
        override fun builtin(id: String, width: Int, height: Int): BackgroundPicture? {
            val pixels = uniffi.lakko_ffi_uniffi.backgroundBuiltinRender(id, width.toUInt(), height.toUInt())
                ?: return null
            if (pixels.size != width * height * 4) return null
            return BackgroundPicture.Rgba(width, height, pixels)
        }

        override fun photo(id: String): BackgroundPicture? {
            val store = photos() ?: return null
            val account = me.lakko.app.contacts.AcceptedContactsStore.boundAccountB64 ?: return null
            val bytes = store.read(account, id) ?: return null
            return BackgroundPhotoStore.decode(bytes)?.let { BackgroundPicture.Image(it) }
        }

        override fun blur(strong: Boolean, frameHeight: Int): BlurParameters {
            val b = uniffi.lakko_ffi_uniffi.backgroundBlur(strong, frameHeight.coerceAtLeast(0).toUInt())
            return BlurParameters(b.divisor.toInt(), b.sigma, b.radius.toInt())
        }

        override fun modelInputRect(frameWidth: Int, frameHeight: Int): FloatArray {
            val r = uniffi.lakko_ffi_uniffi.backgroundModelInputRect(frameWidth.toUInt(), frameHeight.toUInt())
            return floatArrayOf(r.x, r.y, r.width, r.height)
        }

        override fun cover(srcWidth: Int, srcHeight: Int, dstWidth: Int, dstHeight: Int): FloatArray {
            val r = uniffi.lakko_ffi_uniffi.backgroundCoverRect(
                srcWidth.toUInt(), srcHeight.toUInt(), dstWidth.toUInt(), dstHeight.toUInt(),
            )
            return floatArrayOf(r.x, r.y, r.width, r.height)
        }
    }

    // ---- choices --------------------------------------------------------------

    /** The person picked [selection] in the sheet: applies at once. */
    fun select(context: Context, selection: BackgroundSelection) {
        CameraBackgroundSetting.attach(context)
        CameraBackgroundSetting.holder.select(selection)
        if (!selection.isBackground) mutableStatus.value = BackgroundStatus.OFF
    }

    /**
     * The call screen's camera button: off means no frame is sent, so none
     * is processed unless the sheet's preview is open. `true` outside a call
     * screen (the pre-join camera check, a capture before the screen says).
     */
    @Volatile var cameraOn: Boolean = true

    /** The camera was turned back on: a failed background gets another chance. */
    fun cameraTurnedOn() {
        for (fan in liveFans) fan.stage?.requestRestart()
    }

    // ---- fans and preview ---------------------------------------------------

    internal fun fanStarted(fan: CapturerFan) {
        liveFans.addIfAbsent(fan)
    }

    internal fun fanStopped(fan: CapturerFan) {
        liveFans.remove(fan)
    }

    /** Whether a camera is being captured now (the call's, on or off). */
    fun cameraRunning(): Boolean = liveFans.isNotEmpty()

    internal fun deliverPreview(frame: VideoFrame) {
        for (sink in previewSinks) {
            try {
                sink.onFrame(frame)
            } catch (_: Throwable) {
                // A preview that went away mid-frame.
            }
        }
    }

    fun addPreviewSink(sink: VideoSink) {
        previewSinks.addIfAbsent(sink)
    }

    fun removePreviewSink(sink: VideoSink) {
        previewSinks.remove(sink)
    }

    private var previewCapture: CameraVideoCapture? = null
    private var previewSource: org.webrtc.VideoSource? = null

    /**
     * Runs the camera for the sheet's preview only, when no camera is being
     * captured: a source of its own on no track, so nothing is sent (UX
     * section 2.2). Main thread. `false` when no camera could be opened.
     */
    fun startPreviewCamera(context: Context, preferredCamera: String?): Boolean {
        if (previewCapture != null) return true
        if (cameraRunning()) return true
        val source = WebRtcPeerConnection.newVideoSource(context) ?: return false
        val egl = WebRtcPeerConnection.sharedEglContext() ?: run {
            source.dispose()
            return false
        }
        val capture = CameraVideoCapture.create(context, source, egl, preferredCamera) ?: run {
            source.dispose()
            return false
        }
        return try {
            capture.start()
            previewCapture = capture
            previewSource = source
            true
        } catch (cause: Throwable) {
            Log.w(TAG, "preview camera did not start: ${cause.javaClass.simpleName}")
            runCatching { capture.dispose() }
            runCatching { source.dispose() }
            false
        }
    }

    /** Stops the preview-only camera, if one runs. Main thread. */
    fun stopPreviewCamera() {
        val capture = previewCapture ?: return
        previewCapture = null
        runCatching { capture.stop() }
        runCatching { capture.dispose() }
        previewSource?.let { runCatching { it.dispose() } }
        previewSource = null
    }
}

/** Runs [after] once the wrapped stage is released. */
private class ShutdownStage(
    private val inner: CameraFrameStage,
    private val after: () -> Unit,
) : CameraFrameStage by inner {
    override fun release() {
        inner.release()
        after()
    }
}
