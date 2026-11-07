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
    /** Whether the camera this capture was opened on faces the user, which is
     *  what decides whether the self-view starts mirrored. */
    public val startsFrontFacing: Boolean = true,
) {

    /**
     * `true` once `start()` has actually initialised the
     * underlying capturer + told it to begin capturing.
     * Idempotency / safety guard for `stop()`.
     */
    private var didStart: Boolean = false

    /** Hands the capturer's frames to [source] and to every copy's source
     *  ([copySource]); see [CapturerFan] for why each copy has its own. */
    private val fan = CapturerFan(source.capturerObserver)

    /** The sources made for the camera's copies, by copy. Touched on the
     *  call's thread only. */
    private val copySources = java.util.EnumMap<CameraCopyKind, VideoSource>(CameraCopyKind::class.java)

    /**
     * The video source the [kind] copy of the camera is sent from: a source of
     * its own, fed the same frames as the main one, so the copy's encoder can
     * narrow only its own picture ([CapturerFan]). Made on first ask and kept
     * until [dispose]; `null` when no source can be made.
     */
    internal fun copySource(kind: CameraCopyKind): VideoSource? {
        copySources[kind]?.let { return it }
        val made = WebRtcPeerConnection.newVideoSource(context) ?: return null
        copySources[kind] = made
        fan.attach(made.capturerObserver)
        return made
    }

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
            fan,
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
     * Switch to the next camera on the device — `docs/ux/calls.md` section 4.1:
     * "Flip camera does not belong in the row. It lives on the self-view
     * itself, as a small button in that rectangle's corner, where the thing it
     * changes is visible."
     *
     * Before this the flip existed only as a dropdown entry with an empty body,
     * so the menu item did nothing at all. libwebrtc owns the swap: the capture
     * keeps running, the same [VideoSource] keeps receiving frames, and no
     * renegotiation happens, so the remote side sees only the picture change.
     *
     * [onDone] reports whether the swap landed and, when it did, whether the new
     * camera faces the user — which is what decides whether the self-view is
     * mirrored. It is called on libwebrtc's camera thread.
     */
    public fun switchCamera(onDone: (switched: Boolean, isFrontFacing: Boolean) -> Unit) {
        if (!didStart) {
            onDone(false, true)
            return
        }
        try {
            capturer.switchCamera(
                object : CameraVideoCapturer.CameraSwitchHandler {
                    override fun onCameraSwitchDone(isFrontCamera: Boolean) {
                        onDone(true, isFrontCamera)
                    }

                    override fun onCameraSwitchError(errorDescription: String?) {
                        android.util.Log.w(
                            "CameraVideoCapture",
                            "camera switch failed: ${errorDescription.orEmpty()}",
                        )
                        onDone(false, true)
                    }
                },
            )
        } catch (cause: Throwable) {
            android.util.Log.w("CameraVideoCapture", "camera switch threw", cause)
            onDone(false, true)
        }
    }

    /**
     * Switch to the camera named [cameraName] (one of [cameras]): the choice a
     * person makes in the call's Settings sheet. The same swap as
     * [switchCamera], to a named camera instead of the next one; [onDone] has
     * the same meaning and is called on libwebrtc's camera thread.
     */
    public fun switchTo(cameraName: String, onDone: (switched: Boolean, isFrontFacing: Boolean) -> Unit) {
        if (!didStart) {
            onDone(false, true)
            return
        }
        try {
            capturer.switchCamera(
                object : CameraVideoCapturer.CameraSwitchHandler {
                    override fun onCameraSwitchDone(isFrontCamera: Boolean) {
                        onDone(true, isFrontCamera)
                    }

                    override fun onCameraSwitchError(errorDescription: String?) {
                        android.util.Log.w(
                            "CameraVideoCapture",
                            "camera switch failed: ${errorDescription.orEmpty()}",
                        )
                        onDone(false, true)
                    }
                },
                cameraName,
            )
        } catch (cause: Throwable) {
            android.util.Log.w("CameraVideoCapture", "camera switch threw", cause)
            onDone(false, true)
        }
    }

    /**
     * Make the encoder re-initialise by changing the resolution it is being fed
     * and changing it back — the second lever for
     * `docs/ux/calls.md`-section-4.1's mid-call camera-on, when the first one
     * does not reach the encoder.
     *
     * ## Why a second lever exists at all
     *
     * The first is `RtpSender.setParameters`, which makes libwebrtc reconfigure
     * the encoder and a reconfigured encoder keys its next frame. On a live
     * Android-to-iOS call it reported success and no key frame followed, while
     * the receiver asked 78 times and got nothing — so on that stack, at that
     * moment, the parameter write did not reach the encoder.
     *
     * This one takes a different path into the same place: a `VideoSource`
     * output-format change alters the frame size arriving at the encoder, and
     * an encoder whose input resolution changes must re-initialise, which
     * produces an IDR. It touches neither the sender nor its SFrame encryptor,
     * so the stream index and counter are untouched (ADR-0035).
     *
     * Costs a brief dip to three-quarter resolution. That is invisible next to
     * the alternative, which is a picture that never appears at all.
     */
    public suspend fun nudgeOutputFormat() {
        runCatching {
            source.adaptOutputFormat(NUDGE_WIDTH, NUDGE_HEIGHT, TARGET_FPS)
        }.onFailure { cause ->
            android.util.Log.w("CameraVideoCapture", "keyframe: output-format nudge failed", cause)
            return
        }
        kotlinx.coroutines.delay(NUDGE_HOLD_MS)
        runCatching {
            source.adaptOutputFormat(TARGET_WIDTH, TARGET_HEIGHT, TARGET_FPS)
        }.onFailure { cause ->
            // Left at the reduced size, which is a worse picture but still a
            // picture — say so rather than leaving it silent.
            android.util.Log.w("CameraVideoCapture", "keyframe: could not restore the format", cause)
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
        // The copies' sources are this capture's own (the main one is the
        // caller's). A track still on one holds a reference of its own, so
        // releasing ours here is safe before the connection closes.
        for ((_, copy) in copySources) {
            fan.detach(copy.capturerObserver)
            runCatching { copy.dispose() }
        }
        copySources.clear()
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

        /**
         * The reduced size [nudgeOutputFormat] passes through on its way back to
         * [TARGET_WIDTH] x [TARGET_HEIGHT]. Three-quarter scale is far enough
         * from the target that libwebrtc cannot treat the change as a no-op,
         * and near enough that the dip is not worth looking at.
         */
        private const val NUDGE_WIDTH: Int = TARGET_WIDTH * 3 / 4
        private const val NUDGE_HEIGHT: Int = TARGET_HEIGHT * 3 / 4

        /** How long the reduced format is held before restoring it. */
        private const val NUDGE_HOLD_MS: Long = 200L

        private const val SURFACE_HELPER_THREAD_NAME = "lakko-capture"

        /**
         * `true` if at least one camera device exists on this
         * host. Compose call UI should hide the "video" toggle
         * when this is false.
         */
        public fun hasUsableCamera(context: Context): Boolean = cameraCount(context) > 0

        /**
         * How many cameras this device exposes. `docs/ux/calls.md` section 4.1
         * puts the flip button on the self-view; with fewer than two cameras
         * there is nothing to flip to and the button is not drawn.
         */
        public fun cameraCount(context: Context): Int = try {
            Camera2Enumerator(context).deviceNames.size
        } catch (_: Throwable) {
            0
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
            /** The camera chosen in the call's Settings sheet, or `null` for
             *  the default. A name the device no longer has reads as `null`. */
            preferredCamera: String? = null,
        ): CameraVideoCapture? {
            val enumerator: CameraEnumerator = try {
                Camera2Enumerator(context)
            } catch (_: Throwable) {
                return null
            }
            val deviceName = preferredCamera?.takeIf { it in enumerator.deviceNames }
                ?: pickFrontOrFirstCamera(enumerator)
                ?: return null
            val frontFacing = runCatching { enumerator.isFrontFacing(deviceName) }.getOrDefault(true)
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
                startsFrontFacing = frontFacing,
            )
        }

        /** One camera of this device, as the call's Settings sheet lists it. */
        public data class CallCamera(val name: String, val frontFacing: Boolean)

        /**
         * The cameras of this device, the default one first (the front
         * camera, as [create] picks it), or an empty list when they cannot be
         * read.
         */
        public fun cameras(context: Context): List<CallCamera> = try {
            val enumerator = Camera2Enumerator(context)
            val first = pickFrontOrFirstCamera(enumerator)
            enumerator.deviceNames
                .map { CallCamera(name = it, frontFacing = enumerator.isFrontFacing(it)) }
                .sortedBy { if (it.name == first) 0 else 1 }
        } catch (_: Throwable) {
            emptyList()
        }

        /** The camera a call uses when the person chose none. */
        public fun defaultCameraName(context: Context): String? = try {
            pickFrontOrFirstCamera(Camera2Enumerator(context))
        } catch (_: Throwable) {
            null
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
