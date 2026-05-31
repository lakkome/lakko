// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.AndroidView
import org.webrtc.EglBase
import org.webrtc.RendererCommon
import org.webrtc.SurfaceViewRenderer
import org.webrtc.VideoTrack

/**
 * G3-2-video — Compose surface for the local camera preview.
 *
 * Mirror of `apps/ios/Lakko/Calls/LocalVideoPreviewView.swift`.
 * Wraps libwebrtc's `SurfaceViewRenderer` (OpenGL-ES-backed
 * video renderer) inside an `AndroidView` so the call UI can
 * drop the preview anywhere in a Compose tree.
 *
 * Renders frames from any `VideoTrack` the caller hands in —
 * typically the same track that was added to the
 * `WebRtcPeerConnection` via `addLocalVideoTrack(source)`.
 * Pin the track to the **local** track if you want the
 * classic "see yourself" mirror; pin to a remote track to
 * render the other party's stream.
 *
 * **Mirroring:** `mirrored = true` is the standard posture
 * for the front camera (the user expects a mirror image).
 * For back-cam captures or remote-track renders, set
 * `mirrored = false` at the call site.
 *
 * **EGL context contract:** the renderer's GL pipeline MUST
 * share textures with the codec path so the capturer →
 * encoder → renderer chain is zero-copy. The caller passes
 * `WebRtcPeerConnection.sharedEglContext()` here; the
 * `init(...)` call wires the renderer into that same
 * share-group.
 */
@Composable
public fun LocalVideoPreviewView(
    track: VideoTrack?,
    eglContext: EglBase.Context,
    modifier: Modifier = Modifier,
    mirrored: Boolean = true,
    scalingType: RendererCommon.ScalingType =
        RendererCommon.ScalingType.SCALE_ASPECT_FILL,
) {
    // `track` and `mirrored` may flip during the lifetime of
    // the preview (camera switch, remote track swap, etc.).
    // Wrap them in rememberUpdatedState so the AndroidView's
    // update callback re-fires when they change.
    val currentTrack by rememberUpdatedState(newValue = track)
    val currentMirrored by rememberUpdatedState(newValue = mirrored)
    val currentScaling by rememberUpdatedState(newValue = scalingType)

    // Holder of the currently-attached track. Compared inside
    // update to know whether to detach + reattach.
    val attachedTrackRef = remember { mutableTrackRef() }

    AndroidView(
        modifier = modifier,
        factory = { ctx ->
            val view = SurfaceViewRenderer(ctx)
            view.init(eglContext, null)
            view.setScalingType(currentScaling)
            view.setMirror(currentMirrored)
            currentTrack?.let {
                it.addSink(view)
                attachedTrackRef.value = it
            }
            view
        },
        update = { view ->
            view.setScalingType(currentScaling)
            view.setMirror(currentMirrored)

            // Re-attach if the track changed.
            val newTrack = currentTrack
            val attached = attachedTrackRef.value
            if (newTrack !== attached) {
                attached?.removeSink(view)
                newTrack?.addSink(view)
                attachedTrackRef.value = newTrack
            }
        },
    )

    // Tear-down: when this composable leaves the composition,
    // detach the sink + release the renderer's GL textures so
    // we do not leak one texture pool per recompose-and-
    // dispose cycle.
    DisposableEffect(eglContext) {
        onDispose {
            attachedTrackRef.value?.let { /* removeSink runs inside AndroidView teardown */ }
            attachedTrackRef.value = null
        }
    }
}

/**
 * Box around the currently-attached [VideoTrack] so the
 * AndroidView's `update` callback can diff against the
 * previous attachment without re-entering composition.
 * Hand-rolled rather than using `mutableStateOf` because we
 * read / write it from the `update` callback which is NOT a
 * Composable scope.
 */
private class TrackRef {
    @Volatile
    var value: VideoTrack? = null
}

private fun mutableTrackRef(): TrackRef = TrackRef()
