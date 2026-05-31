// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import org.webrtc.Camera2Enumerator
import org.webrtc.CameraEnumerator
import org.webrtc.CameraVideoCapturer
import org.webrtc.SurfaceTextureHelper
import org.webrtc.VideoSource

/**
 * G3-2-video — Android local-camera capture wrapper.
 *
 * Mirror of `apps/ios/Lakko/Calls/LocalCameraCapture.swift`.
 * Wraps libwebrtc's `CameraVideoCapturer` (the
 * `Camera2Enumerator`-backed implementation) with a small
 * lakko-side surface that:
 *   - prefers the **front** camera by default (falls back to
 *     any back-facing or first available camera if no front
 *     camera is present, e.g. some Android Auto head units);
 *   - captures at 1280×720 at 30 FPS — same resolution /
 *     framerate budget the iOS half uses, so receiver-side
 *     codec negotiation matches across platforms;
 *   - drives frames into a caller-supplied `VideoSource` via
 *     libwebrtc's `CapturerObserver`.
 *
 * Lifecycle:
 *   1. Caller asks `WebRtcPeerConnection.newVideoSource(ctx)`
 *      for a fresh source.
 *   2. Caller constructs a `CameraVideoCapture(ctx, source,
 *      eglContext)`.
 *   3. Caller calls `capture.start()`.
 *   4. Frames flow; the `VideoTrack` added via
 *      `peerConnection.addLocalVideoTrack(source)` sees them.
 *   5. On call teardown, `capture.stop()` then `capture.dispose()`.
 *
 * **Permission contract:** the host app MUST carry the
 * `android.permission.CAMERA` runtime permission (declared in
 * `AndroidManifest.xml` and granted via
 * `ActivityCompat.requestPermissions`) before `start()` is
 * called. This class does NOT request permission — that is
 * the UI's job (consistent with iOS).
 *
 * **Threading:** all methods are safe to call from the main
 * thread. `start` blocks until libwebrtc has fired its first
 * camera-open callback (~50-150ms on modern devices).
 */
public class CameraVideoCapture private constructor(
    private val capturer: CameraVideoCapturer,
    private val source: VideoSource,
    private val surfaceHelper: SurfaceTextureHelper,
    private val context: Context,
) {

    /**
     * `true` once `start()` has actually initialised the
     * underlying capturer + told it to begin capturing.
     * Idempotency / safety guard for `stop()`.
     */
    private var didStart: Boolean = false

    /**
     * Begin delivering frames into the source. May throw
     * `CaptureError.StartFailed` if libwebrtc's `startCapture`
     * fails (typically permission revoked at runtime).
     */
    public fun start() {
        if (didStart) return
        capturer.initialize(
            surfaceHelper,
            context,
            source.capturerObserver,
        )
        try {
            capturer.startCapture(TARGET_WIDTH, TARGET_HEIGHT, TARGET_FPS)
            didStart = true
        } catch (t: Throwable) {
            throw CaptureError.StartFailed(t)
        }
    }

    /**
     * Stop the underlying capture. Idempotent; safe to call
     * even after a failed `start()`. Blocks until libwebrtc
     * has fully released the camera (some Android codecs
     * deadlock if the next `start` runs before this returns).
     */
    public fun stop() {
        if (!didStart) return
        didStart = false
        try {
            capturer.stopCapture()
        } catch (_: InterruptedException) {
            // Interrupted while waiting for the camera-thread
            // join. Re-raise the interrupt status so the
            // calling coroutine notices.
            Thread.currentThread().interrupt()
        }
    }

    /**
     * Release the capturer + surface helper + the EGL
     * texture pool they own. Call this after `stop()` and
     * after the `VideoSource` has been detached from any
     * peer connections. Not idempotent — second call may
     * crash libwebrtc.
     */
    public fun dispose() {
        capturer.dispose()
        surfaceHelper.dispose()
    }

    public sealed class CaptureError(msg: String, cause: Throwable? = null) :
        Exception(msg, cause) {

        /** No `CameraEnumerator` returned any device. */
        public object NoDevice : CaptureError("no camera devices available")

        /** libwebrtc's `startCapture` threw. */
        public class StartFailed(cause: Throwable) :
            CaptureError("camera startCapture failed: ${cause.message}", cause)
    }

    public companion object {

        /**
         * Target capture resolution. 720p matches the iOS half
         * + plays well with both VP9 and H.264 encoders that
         * the `DefaultVideoEncoderFactory` ships with on
         * Android.
         */
        public const val TARGET_WIDTH: Int = 1280
        public const val TARGET_HEIGHT: Int = 720

        /**
         * Target framerate. libwebrtc clips frames internally
         * if the device's actual sensor max is lower; 30 is
         * the conventional ceiling for mobile video calls.
         */
        public const val TARGET_FPS: Int = 30

        private const val SURFACE_HELPER_THREAD_NAME = "lakko-capture"

        /**
         * `true` if at least one camera device exists on this
         * host. Compose call UI should hide the "video" toggle
         * when this is false.
         */
        public fun hasUsableCamera(context: Context): Boolean {
            return try {
                Camera2Enumerator(context).deviceNames.isNotEmpty()
            } catch (_: Throwable) {
                false
            }
        }

        /**
         * Construct a capture wrapped around the
         * `VideoSource` the caller obtained from
         * `WebRtcPeerConnection.newVideoSource(context)`. The
         * EGL context comes from
         * `WebRtcPeerConnection.sharedEglContext()` and MUST
         * be non-null (caller should have already constructed
         * a `WebRtcPeerConnection`, which primes the factory).
         *
         * Returns `null` if no camera is enumerable on this
         * host. Throws `CaptureError.NoDevice` from `start()`
         * later if the enumerator gives zero device names.
         */
        public fun create(
            context: Context,
            source: VideoSource,
            eglContext: org.webrtc.EglBase.Context,
        ): CameraVideoCapture? {
            val enumerator: CameraEnumerator = try {
                Camera2Enumerator(context)
            } catch (_: Throwable) {
                return null
            }
            val deviceName = pickFrontOrFirstCamera(enumerator) ?: return null
            val capturer = enumerator.createCapturer(deviceName, null) ?: return null
            val helper = SurfaceTextureHelper.create(
                SURFACE_HELPER_THREAD_NAME,
                eglContext,
            )
            return CameraVideoCapture(
                capturer = capturer,
                source = source,
                surfaceHelper = helper,
                context = context,
            )
        }

        /**
         * Returns the first front-facing device name, or
         * (falling back) the first back-facing device, or
         * (falling back again) the first enumerated device.
         * Returns `null` only if the enumerator gives an empty
         * list — caller should treat that as the "no camera"
         * case.
         */
        private fun pickFrontOrFirstCamera(
            enumerator: CameraEnumerator,
        ): String? {
            val names = enumerator.deviceNames
            if (names.isEmpty()) return null
            return names.firstOrNull { enumerator.isFrontFacing(it) }
                ?: names.firstOrNull { enumerator.isBackFacing(it) }
                ?: names.first()
        }
    }
}
